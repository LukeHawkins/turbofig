/**
 * Benchmark harness.
 *
 * Runs one scenario against one target (--target turbofig-mcp |
 * turbofig-bridge | console-mcp) N times (default 10), reports cold
 * (first run) and warm (remaining runs) timing separately, and writes a
 * full JSON report. A run with any failed job is marked invalid. Only one
 * Figma plugin runs per file at a time, so the harness drives exactly one
 * target per invocation; `bun bench/compare.ts` merges reports written by
 * separate target runs.
 *
 * Pure functions: median, p95, summarizeJobs, compareToBaseline.
 * CLI: run with `bun bench/harness.ts --dry-run --target turbofig-bridge`.
 */

import { homedir } from "node:os";
import { join } from "node:path";
import type { BridgeJob, Scenario } from "./scenarios.js";
import { scenarios } from "./scenarios.js";
import { defaultMcpUrl, isTargetId, type TargetId, targetDescription } from "./targets.js";
import {
  consoleMcpAdapter,
  fileBridgeTransport,
  mcpTransport,
  stubTransport,
  type Transport,
  turbofigMcpAdapter,
} from "./transports.js";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/** One job's measured outcome within one iteration of a scenario run. */
export interface JobRecord {
  role: "setup" | "job" | "teardown";
  /** Index into scenario.jobs for role "job"; -1 for setup/teardown. */
  index: number;
  ok: boolean;
  wallMs: number;
  requestBytes: number;
  responseBytes: number;
  error?: string;
}

/** One full run of a scenario's job list (setup + jobs + teardown). */
export interface IterationRecord {
  iteration: number;
  cold: boolean;
  valid: boolean;
  records: JobRecord[];
}

/** Per-job timing distribution across all warm iterations. */
export interface JobStat {
  index: number;
  coldMs: number | null;
  warmMedianMs: number | null;
  warmP95Ms: number | null;
  requestBytes: number;
  responseBytes: number;
}

/** Full report written to --out file. One shape, used for both a live report
 * and a saved baseline, so --baseline always reads what --out wrote. */
export interface BenchReport {
  scenario: string;
  label: "transport" | "transport + helpers";
  target: TargetId;
  runs: number;
  timeoutMs: number;
  valid: boolean;
  invalidReason: string | null;
  /** Set only on a static, non-live baseline (e.g. a dry-run payload-size
   * snapshot committed as bench/baseline.json). Names what the numbers do
   * and do not represent, so nobody mistakes a payload-size snapshot for a
   * live timing measurement. Absent on every real `--target` run. */
  note?: string;
  jobStats: JobStat[];
  totalRequestBytes: number;
  totalResponseBytes: number;
  iterations: IterationRecord[];
  machine: Record<string, string | null>;
  timestamp: string;
}

// ---------------------------------------------------------------------------
// Pure functions
// ---------------------------------------------------------------------------

/** Median of a non-empty numeric array. */
export function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid];
}

/** 95th percentile of a non-empty numeric array (nearest-rank method). */
export function p95(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const rank = Math.ceil(0.95 * sorted.length) - 1;
  return sorted[Math.max(0, Math.min(rank, sorted.length - 1))];
}

/**
 * Build per-job stats from a set of iteration records.
 * coldMs comes from iteration 0 only. warmMedianMs/warmP95Ms come from every
 * iteration after the first. Bytes are taken from the first iteration that
 * has a record for that job (payload size does not vary run to run).
 */
export function summarizeJobs(iterations: IterationRecord[], jobCount: number): JobStat[] {
  const stats: JobStat[] = [];
  for (let i = 0; i < jobCount; i++) {
    const coldRecord = iterations[0]?.records.find((r) => r.role === "job" && r.index === i);
    const warmTimes: number[] = [];
    let requestBytes = coldRecord?.requestBytes ?? 0;
    let responseBytes = coldRecord?.responseBytes ?? 0;
    for (const iter of iterations.slice(1)) {
      const rec = iter.records.find((r) => r.role === "job" && r.index === i);
      if (rec) {
        warmTimes.push(rec.wallMs);
        requestBytes = requestBytes || rec.requestBytes;
        responseBytes = responseBytes || rec.responseBytes;
      }
    }
    stats.push({
      index: i,
      coldMs: coldRecord ? coldRecord.wallMs : null,
      warmMedianMs: warmTimes.length > 0 ? median(warmTimes) : null,
      warmP95Ms: warmTimes.length > 0 ? p95(warmTimes) : null,
      requestBytes,
      responseBytes,
    });
  }
  return stats;
}

/**
 * Compare a run's total wire bytes (request + response) against a baseline
 * report for the same scenario and target.
 *
 * Returns the ratio as a fixed-3 string when the scenario and target match
 * and the baseline total is positive. Returns "no valid baseline total" when
 * the baseline total is zero, negative, or not a number. Returns null when
 * the scenario or target does not match (no comparison applies).
 */
export function compareToBaseline(report: BenchReport, baseline: BenchReport): string | null {
  if (baseline.scenario !== report.scenario || baseline.target !== report.target) return null;
  const baselineTotal = baseline.totalRequestBytes + baseline.totalResponseBytes;
  if (typeof baselineTotal !== "number" || Number.isNaN(baselineTotal) || baselineTotal <= 0) {
    return "no valid baseline total";
  }
  const thisTotal = report.totalRequestBytes + report.totalResponseBytes;
  return (thisTotal / baselineTotal).toFixed(3);
}

// ---------------------------------------------------------------------------
// Scenario isolation: a fresh page per iteration, created and removed.
// Never touches the user's existing pages or content. documentAccess is
// "dynamic-page" (see plugin/manifest.json), so page switches are async.
// ---------------------------------------------------------------------------

function setupJob(pageName: string, fileKey: string | undefined): BridgeJob {
  return {
    op: "execute",
    fileKey,
    code: `
const _p = figma.createPage();
_p.name = ${JSON.stringify(pageName)};
await figma.setCurrentPageAsync(_p);
return { pageId: _p.id };
    `.trim(),
  };
}

function teardownJob(fileKey: string | undefined): BridgeJob {
  return {
    op: "execute",
    fileKey,
    code: `
const _p = figma.currentPage;
const sibling = figma.root.children.find((p) => p.id !== _p.id);
if (sibling) await figma.setCurrentPageAsync(sibling);
_p.remove();
return { removed: true };
    `.trim(),
  };
}

// ---------------------------------------------------------------------------
// Run one scenario N times against one target
// ---------------------------------------------------------------------------

async function runOneIteration(
  scenario: Scenario,
  transport: Transport,
  iteration: number,
  fileKey: string | undefined,
): Promise<IterationRecord> {
  const records: JobRecord[] = [];

  async function run(job: BridgeJob, role: JobRecord["role"], index: number): Promise<boolean> {
    try {
      const { ok, wallMs, requestBytes, responseBytes } = await transport.submit(job);
      records.push({ role, index, ok, wallMs, requestBytes, responseBytes });
      return ok;
    } catch (err) {
      records.push({
        role,
        index,
        ok: false,
        wallMs: 0,
        requestBytes: 0,
        responseBytes: 0,
        error: err instanceof Error ? err.message : String(err),
      });
      return false;
    }
  }

  const setupOk = await run(setupJob(scenario.pageName, fileKey), "setup", -1);
  if (setupOk) {
    for (let i = 0; i < scenario.jobs.length; i++) {
      // A --file-key override applies to every job in the scenario, not only
      // setup/teardown, so a job never falls back to the ambiguous "sole
      // connected file" rule when two files are open.
      const job = fileKey !== undefined ? { ...scenario.jobs[i], fileKey } : scenario.jobs[i];
      await run(job, "job", i);
    }
    await run(teardownJob(fileKey), "teardown", -1);
  }

  const valid = records.every((r) => r.ok);
  return { iteration, cold: iteration === 0, valid, records };
}

export async function runScenarioAgainstTarget(
  scenario: Scenario,
  transport: Transport,
  runs: number,
  fileKey: string | undefined,
): Promise<{ iterations: IterationRecord[]; valid: boolean; invalidReason: string | null }> {
  await transport.init?.();
  const iterations: IterationRecord[] = [];
  let invalidReason: string | null = null;
  try {
    for (let i = 0; i < runs; i++) {
      const iter = await runOneIteration(scenario, transport, i, fileKey);
      iterations.push(iter);
      if (!iter.valid && invalidReason === null) {
        const failed = iter.records.find((r) => !r.ok);
        invalidReason = `iteration ${i}: ${failed?.role ?? "job"} ${failed?.index ?? ""} failed${
          failed?.error ? `: ${failed.error}` : ""
        }`;
      }
    }
  } finally {
    await transport.close?.();
  }
  return { iterations, valid: invalidReason === null, invalidReason };
}

function buildTransport(target: TargetId, opts: ResolvedArgs): Transport {
  if (opts.dryRun) return stubTransport();
  switch (target) {
    case "turbofig-bridge":
      return fileBridgeTransport(opts.bridgeDir, opts.timeoutMs);
    case "turbofig-mcp":
      return mcpTransport(opts.mcpUrl ?? defaultMcpUrl(target), turbofigMcpAdapter, opts.timeoutMs);
    case "console-mcp":
      return mcpTransport(opts.mcpUrl ?? defaultMcpUrl(target), consoleMcpAdapter, opts.timeoutMs);
  }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

interface ResolvedArgs {
  scenarioName: string;
  target: TargetId | null;
  bridgeDir: string;
  mcpUrl: string | null;
  fileKey: string | undefined;
  runs: number;
  timeoutMs: number;
  outFile: string | null;
  dryRun: boolean;
  baselineFile: string | null;
  machine: Record<string, string | null>;
}

const DEFAULT_RUNS = 10;
/** Longer than the daemon's default 30s request timeout, so the harness
 * itself never clips a slower-but-successful call short. */
const DEFAULT_TIMEOUT_MS = 45_000;

function parseArgs(args: string[]): ResolvedArgs {
  let scenarioName = "all";
  let target: TargetId | null = null;
  let bridgeDir = join(homedir(), ".turbofig");
  let mcpUrl: string | null = null;
  let fileKey: string | undefined;
  let runs = DEFAULT_RUNS;
  let timeoutMs = DEFAULT_TIMEOUT_MS;
  let outFile: string | null = null;
  let dryRun = false;
  let baselineFile: string | null = null;
  const machine: Record<string, string | null> = {
    machine: null,
    macos: null,
    figmaVersion: null,
    daemonVersion: null,
    file: null,
  };

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--scenario" && args[i + 1]) scenarioName = args[++i];
    else if (arg === "--target" && args[i + 1]) {
      const t = args[++i];
      if (!isTargetId(t)) {
        throw new Error(
          `Unknown --target: ${t}. Choose turbofig-mcp, turbofig-bridge, or console-mcp.`,
        );
      }
      target = t;
    } else if (arg === "--bridge-dir" && args[i + 1]) bridgeDir = args[++i];
    else if (arg === "--mcp-url" && args[i + 1]) mcpUrl = args[++i];
    else if (arg === "--file-key" && args[i + 1]) fileKey = args[++i];
    else if (arg === "--runs" && args[i + 1]) runs = Number.parseInt(args[++i], 10);
    else if (arg === "--timeout" && args[i + 1]) timeoutMs = Number.parseInt(args[++i], 10);
    else if (arg === "--out" && args[i + 1]) outFile = args[++i];
    else if (arg === "--dry-run") dryRun = true;
    else if (arg === "--baseline" && args[i + 1]) baselineFile = args[++i];
    else if (arg === "--machine" && args[i + 1]) machine.machine = args[++i];
    else if (arg === "--macos" && args[i + 1]) machine.macos = args[++i];
    else if (arg === "--figma-version" && args[i + 1]) machine.figmaVersion = args[++i];
    else if (arg === "--daemon-version" && args[i + 1]) machine.daemonVersion = args[++i];
    else if (arg === "--file" && args[i + 1]) machine.file = args[++i];
  }

  if (!dryRun && !target) {
    throw new Error("Pass --target turbofig-mcp | turbofig-bridge | console-mcp (or --dry-run).");
  }

  return {
    scenarioName,
    target,
    bridgeDir,
    mcpUrl,
    fileKey,
    runs,
    timeoutMs,
    outFile,
    dryRun,
    baselineFile,
    machine,
  };
}

function printReport(report: BenchReport): void {
  console.log(`\nScenario: ${report.scenario} (${report.label}) on ${report.target}`);
  console.log(`  Runs       : ${report.runs}${report.valid ? "" : " -- INVALID"}`);
  if (!report.valid) console.log(`  Invalid    : ${report.invalidReason}`);
  console.log(`  Req bytes  : ${report.totalRequestBytes}`);
  console.log(`  Resp bytes : ${report.totalResponseBytes}`);
  for (const stat of report.jobStats) {
    console.log(
      `  Job ${stat.index}: cold ${stat.coldMs ?? "-"}ms, warm median ${stat.warmMedianMs ?? "-"}ms, ` +
        `warm p95 ${stat.warmP95Ms ?? "-"}ms, req ${stat.requestBytes}B, resp ${stat.responseBytes}B`,
    );
  }
}

async function printBaseline(report: BenchReport, baselineFile: string): Promise<void> {
  const file = Bun.file(baselineFile);
  if (!(await file.exists())) {
    console.log(`\nBaseline file not found: ${baselineFile}. Skipping comparison.`);
    return;
  }
  let baseline: BenchReport;
  try {
    baseline = (await file.json()) as BenchReport;
  } catch {
    console.log("\nMalformed baseline, skipping.");
    return;
  }
  console.log(`\nBaseline comparison (${baselineFile}):`);
  const ratio = compareToBaseline(report, baseline);
  if (ratio === null) {
    console.log(
      `  Scenario/target mismatch: baseline is "${baseline.scenario}"/"${baseline.target}", ` +
        `this run is "${report.scenario}"/"${report.target}". Skipping ratio.`,
    );
  } else if (ratio === "no valid baseline total") {
    console.log("  no valid baseline total");
  } else {
    console.log(`  Ratio (this / baseline) for req+resp bytes: ${ratio}`);
  }
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const target: TargetId = args.target ?? "turbofig-bridge"; // dry-run default label only
  const names: string[] =
    args.scenarioName === "all" ? Object.keys(scenarios) : [args.scenarioName];

  const reports: BenchReport[] = [];

  for (const name of names) {
    const scenario = scenarios[name];
    if (!scenario) {
      console.error(`Unknown scenario: ${name}. Choose from: ${Object.keys(scenarios).join(", ")}`);
      process.exitCode = 1;
      continue;
    }
    if (!args.dryRun && !scenario.targets.includes(target)) {
      console.error(
        `Scenario "${name}" does not support target "${target}" (supports: ${scenario.targets.join(", ")}). Skipping.`,
      );
      continue;
    }

    console.log(
      `\nRunning scenario: ${name}${args.dryRun ? " (dry-run)" : ` on ${targetDescription(target)}`}`,
    );
    const transport = buildTransport(target, args);
    const { iterations, valid, invalidReason } = await runScenarioAgainstTarget(
      scenario,
      transport,
      args.runs,
      args.fileKey,
    );
    const jobStats = summarizeJobs(iterations, scenario.jobs.length);
    const totalRequestBytes = jobStats.reduce((sum, s) => sum + s.requestBytes, 0);
    const totalResponseBytes = jobStats.reduce((sum, s) => sum + s.responseBytes, 0);

    const report: BenchReport = {
      scenario: name,
      label: scenario.label,
      target,
      runs: args.runs,
      timeoutMs: args.timeoutMs,
      valid,
      invalidReason,
      jobStats,
      totalRequestBytes,
      totalResponseBytes,
      iterations,
      machine: args.machine,
      timestamp: new Date().toISOString(),
    };
    printReport(report);
    if (args.baselineFile) await printBaseline(report, args.baselineFile);
    reports.push(report);

    // Write a partial report after every scenario, not only at the end, so a
    // later scenario's failure never discards an earlier scenario's data.
    if (args.outFile) {
      await Bun.write(args.outFile, JSON.stringify(reports, null, 2));
    }
  }

  if (args.outFile) {
    console.log(`\nReport written to: ${args.outFile}`);
  }
}

if (import.meta.main) {
  try {
    await main();
  } catch (err: unknown) {
    console.error("Benchmark failed:", err instanceof Error ? err.message : String(err));
    process.exitCode = 1;
  }
}
