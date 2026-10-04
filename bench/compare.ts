/**
 * Merge BenchReport[] files from separate --target runs into one comparison
 * table. Only one Figma plugin runs per file at a time, so the harness
 * benchmarks one target per invocation; this command is the seam that joins
 * turbofig-mcp, turbofig-bridge, and console-mcp results back together.
 *
 * Usage:
 *   bun bench/compare.ts --report out/turbofig-mcp.json --report out/turbofig-bridge.json --report out/console-mcp.json
 *   bun bench/compare.ts --report ... --out compare.json
 */

import type { BenchReport, JobStat } from "./harness.js";
import type { TargetId } from "./targets.js";

/** One target's summary for one scenario. */
export interface TargetSummary {
  target: TargetId;
  valid: boolean;
  invalidReason: string | null;
  totalRequestBytes: number;
  totalResponseBytes: number;
  warmMedianMsSum: number | null;
  coldMsSum: number | null;
}

/** One scenario's summary across every target that ran it. */
export interface ComparisonRow {
  scenario: string;
  label: "transport" | "transport + helpers";
  perTarget: Partial<Record<TargetId, TargetSummary>>;
}

function sumJobStat(jobStats: JobStat[], field: "warmMedianMs" | "coldMs"): number | null {
  const values = jobStats.map((s) => s[field]);
  if (values.some((v) => v === null)) return null;
  return (values as number[]).reduce((a, b) => a + b, 0);
}

/** Flatten every BenchReport into one row per scenario, one column per target. */
export function buildComparison(reports: BenchReport[]): ComparisonRow[] {
  const rows = new Map<string, ComparisonRow>();
  for (const report of reports) {
    let row = rows.get(report.scenario);
    if (!row) {
      row = { scenario: report.scenario, label: report.label, perTarget: {} };
      rows.set(report.scenario, row);
    }
    row.perTarget[report.target] = {
      target: report.target,
      valid: report.valid,
      invalidReason: report.invalidReason,
      totalRequestBytes: report.totalRequestBytes,
      totalResponseBytes: report.totalResponseBytes,
      warmMedianMsSum: sumJobStat(report.jobStats, "warmMedianMs"),
      coldMsSum: sumJobStat(report.jobStats, "coldMs"),
    };
  }
  return [...rows.values()].sort((a, b) => a.scenario.localeCompare(b.scenario));
}

/** Ratio of one target's total wire bytes to another target's, for the same
 * scenario. Returns null when either side is missing or invalid, so an
 * invalid run never silently contributes a number to the published ratio. */
export function byteRatio(
  row: ComparisonRow,
  numerator: TargetId,
  denominator: TargetId,
): number | null {
  const num = row.perTarget[numerator];
  const den = row.perTarget[denominator];
  if (!num || !den || !num.valid || !den.valid) return null;
  const denTotal = den.totalRequestBytes + den.totalResponseBytes;
  if (denTotal <= 0) return null;
  const numTotal = num.totalRequestBytes + num.totalResponseBytes;
  return numTotal / denTotal;
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(args: string[]): { reportFiles: string[]; outFile: string | null } {
  const reportFiles: string[] = [];
  let outFile: string | null = null;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--report" && args[i + 1]) reportFiles.push(args[++i]);
    else if (args[i] === "--out" && args[i + 1]) outFile = args[++i];
  }
  return { reportFiles, outFile };
}

function printRow(row: ComparisonRow): void {
  console.log(`\nScenario: ${row.scenario} (${row.label})`);
  for (const summary of Object.values(row.perTarget)) {
    if (!summary) continue;
    const status = summary.valid ? "valid" : `INVALID: ${summary.invalidReason}`;
    console.log(
      `  ${summary.target.padEnd(16)} ${status} — req ${summary.totalRequestBytes}B, resp ${summary.totalResponseBytes}B, ` +
        `cold sum ${summary.coldMsSum ?? "-"}ms, warm median sum ${summary.warmMedianMsSum ?? "-"}ms`,
    );
  }
  const ratio = byteRatio(row, "turbofig-bridge", "console-mcp");
  if (ratio !== null) {
    console.log(`  turbofig-bridge / console-mcp bytes ratio: ${ratio.toFixed(3)}`);
  }
}

async function main(): Promise<void> {
  const { reportFiles, outFile } = parseArgs(process.argv.slice(2));
  if (reportFiles.length === 0) {
    throw new Error(
      "Pass at least one --report <file.json> (repeat the flag for each target's report).",
    );
  }

  const allReports: BenchReport[] = [];
  for (const path of reportFiles) {
    const file = Bun.file(path);
    if (!(await file.exists())) {
      throw new Error(`Report file not found: ${path}`);
    }
    const parsed = (await file.json()) as BenchReport[] | BenchReport;
    allReports.push(...(Array.isArray(parsed) ? parsed : [parsed]));
  }

  const rows = buildComparison(allReports);
  for (const row of rows) printRow(row);

  if (outFile) {
    await Bun.write(outFile, JSON.stringify(rows, null, 2));
    console.log(`\nComparison written to: ${outFile}`);
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (err: unknown) {
    console.error("Compare failed:", err instanceof Error ? err.message : String(err));
    process.exitCode = 1;
  }
}
