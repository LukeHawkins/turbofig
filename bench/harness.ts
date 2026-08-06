/**
 * Benchmark harness for turbofig file-bridge scenarios.
 *
 * Pure functions: estimateTokens, jobTokens, summarizeRun, runScenario, compareToBaseline.
 * Transport: fileBridgeTransport (live) and a stub for dry-run / tests.
 * CLI: run with `bun bench/harness.ts --dry-run`.
 */

import { randomUUID } from "node:crypto";
import { unlink } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import type { BridgeJob, Scenario } from "./scenarios.js";
import { scenarios } from "./scenarios.js";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/** One job's timing and token measurement. */
export interface RunRecord {
  tokensIn: number;
  tokensOut: number;
  wallMs: number;
}

/** Aggregate totals for a complete scenario run. */
export interface RunSummary {
  tokensIn: number;
  tokensOut: number;
  totalTokens: number;
  wallMs: number;
  count: number;
}

/** Injectable transport for testability. */
export interface Transport {
  submit(job: BridgeJob): Promise<{ result: unknown; wallMs: number }>;
}

/** Shape of a baseline file written by a prior run or external tool. */
export interface BaselineReport {
  scenario: string;
  totalTokens: number;
}

/** Full report written to --out file. */
export interface BenchReport {
  scenario: string;
  summary: RunSummary;
  records: RunRecord[];
  timestamp: string;
}

// ---------------------------------------------------------------------------
// Pure functions
// ---------------------------------------------------------------------------

/**
 * Estimate token count from a text string.
 * Uses the standard rough approximation: one token per four characters.
 */
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

/** Count the tokens in a serialised file-bridge job. */
export function jobTokens(job: BridgeJob): number {
  return estimateTokens(JSON.stringify(job));
}

/** Sum a list of RunRecords into a RunSummary. */
export function summarizeRun(records: RunRecord[]): RunSummary {
  let tokensIn = 0;
  let tokensOut = 0;
  let wallMs = 0;
  for (const r of records) {
    tokensIn += r.tokensIn;
    tokensOut += r.tokensOut;
    wallMs += r.wallMs;
  }
  return {
    tokensIn,
    tokensOut,
    totalTokens: tokensIn + tokensOut,
    wallMs,
    count: records.length,
  };
}

/**
 * Run all jobs in a scenario through the given transport.
 * Returns the aggregate summary and per-job records.
 */
export async function runScenario(
  scenario: Scenario,
  deps: Transport,
): Promise<{ summary: RunSummary; records: RunRecord[] }> {
  const records: RunRecord[] = [];
  for (const job of scenario.jobs) {
    const tokensIn = jobTokens(job);
    const { result, wallMs } = await deps.submit(job);
    const tokensOut = estimateTokens(JSON.stringify(result));
    records.push({ tokensIn, tokensOut, wallMs });
  }
  return { summary: summarizeRun(records), records };
}

/**
 * Compare a run summary against a baseline for the same scenario.
 *
 * Returns the ratio as a fixed-3 string when the scenario matches and the
 * baseline has a positive totalTokens. Returns "no valid baseline total" when
 * totalTokens is zero, negative, or not a number. Returns null when the
 * scenario name does not match (no comparison applies).
 */
export function compareToBaseline(
  name: string,
  summary: RunSummary,
  baseline: BaselineReport,
): string | null {
  if (baseline.scenario !== name) return null;
  if (
    typeof baseline.totalTokens !== "number" ||
    Number.isNaN(baseline.totalTokens) ||
    baseline.totalTokens <= 0
  ) {
    return "no valid baseline total";
  }
  return (summary.totalTokens / baseline.totalTokens).toFixed(3);
}

// ---------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------

/** Poll interval for outbox file detection, in milliseconds. */
const POLL_INTERVAL_MS = 100;

/** Maximum wait for one job result before throwing, in milliseconds. */
const POLL_TIMEOUT_MS = 30_000;

/**
 * Real file-bridge transport.
 * Writes each job to inbox/<id>.json and polls outbox/<id>.json for the result.
 * Uses Bun.write / Bun.file for file I/O and node:crypto for job IDs.
 * Deletes both files in a finally block to prevent temp-file leaks.
 * Treats a JSON parse error during polling as a partial write and retries.
 */
export function fileBridgeTransport(bridgeDir: string): Transport {
  return {
    async submit(job: BridgeJob): Promise<{ result: unknown; wallMs: number }> {
      const id = randomUUID();
      const inboxPath = join(bridgeDir, "inbox", `${id}.json`);
      const outboxPath = join(bridgeDir, "outbox", `${id}.json`);

      await Bun.write(inboxPath, JSON.stringify(job));

      const start = Date.now();
      try {
        while (true) {
          const file = Bun.file(outboxPath);
          if (await file.exists()) {
            const text = await file.text();
            let parsed: unknown;
            try {
              parsed = JSON.parse(text);
            } catch {
              // Partial write detected. Keep polling until the file is complete.
              await Bun.sleep(POLL_INTERVAL_MS);
              continue;
            }
            const wallMs = Date.now() - start;
            return { result: parsed, wallMs };
          }
          if (Date.now() - start > POLL_TIMEOUT_MS) {
            throw new Error(`Timeout waiting for bridge result: job ${id}`);
          }
          await Bun.sleep(POLL_INTERVAL_MS);
        }
      } finally {
        // Delete inbox file. It is always written, so ignore any error.
        try {
          await unlink(inboxPath);
        } catch {
          /* ignore */
        }
        // Delete outbox file. It may not exist when the job timed out.
        try {
          await unlink(outboxPath);
        } catch {
          /* ignore missing file */
        }
      }
    },
  };
}

/**
 * Stub transport for dry-run mode.
 * Returns a canned small result with wallMs 0.
 * No daemon required.
 */
function stubTransport(cannedResult: unknown = { ok: true }): Transport {
  return {
    async submit(_job: BridgeJob): Promise<{ result: unknown; wallMs: number }> {
      return { result: cannedResult, wallMs: 0 };
    },
  };
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/** Print a compact summary table to stdout. */
function printSummary(scenario: string, summary: RunSummary): void {
  console.log(`\nScenario: ${scenario}`);
  console.log(`  Jobs       : ${summary.count}`);
  console.log(`  Tokens in  : ${summary.tokensIn}`);
  console.log(`  Tokens out : ${summary.tokensOut}`);
  console.log(`  Total tok  : ${summary.totalTokens}`);
  console.log(`  Wall ms    : ${summary.wallMs}`);
}

/**
 * Compare a run summary against a saved baseline for the given scenario name
 * and print the result. Handles a missing file, a malformed file, a scenario
 * mismatch, and an invalid totalTokens without throwing.
 */
async function printBaseline(
  name: string,
  baselineFile: string,
  summary: RunSummary,
): Promise<void> {
  const file = Bun.file(baselineFile);
  const exists = await file.exists();
  if (!exists) {
    console.log(`\nBaseline file not found: ${baselineFile}. Skipping comparison.`);
    return;
  }
  let baseline: BaselineReport;
  try {
    baseline = (await file.json()) as BaselineReport;
  } catch {
    console.log("\nmalformed baseline, skipping");
    return;
  }
  console.log(`\nBaseline comparison (${baselineFile}):`);
  console.log(`  Baseline scenario      : ${baseline.scenario}`);
  console.log(`  Baseline total tokens  : ${baseline.totalTokens}`);
  console.log(`  This run total tokens  : ${summary.totalTokens}`);
  const ratio = compareToBaseline(name, summary, baseline);
  if (ratio === null) {
    console.log(
      `  Scenario mismatch: baseline is "${baseline.scenario}", this run is "${name}". Skipping ratio.`,
    );
  } else if (ratio === "no valid baseline total") {
    console.log(`  no valid baseline total`);
  } else {
    console.log(`  Ratio (this / baseline): ${ratio}`);
  }
}

/** Parse the CLI arguments. */
function parseArgs(args: string[]): {
  scenarioName: string;
  bridgeDir: string;
  outFile: string | null;
  dryRun: boolean;
  baselineFile: string | null;
} {
  let scenarioName = "all";
  let bridgeDir = join(homedir(), ".turbofig");
  let outFile: string | null = null;
  let dryRun = false;
  let baselineFile: string | null = null;

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--scenario" && args[i + 1]) {
      scenarioName = args[++i];
    } else if (arg === "--bridge-dir" && args[i + 1]) {
      bridgeDir = args[++i];
    } else if (arg === "--out" && args[i + 1]) {
      outFile = args[++i];
    } else if (arg === "--dry-run") {
      dryRun = true;
    } else if (arg === "--baseline" && args[i + 1]) {
      baselineFile = args[++i];
    }
  }

  return { scenarioName, bridgeDir, outFile, dryRun, baselineFile };
}

/** Entry point. Guarded by import.meta.main so imports do not trigger it. */
async function main(): Promise<void> {
  const args = process.argv.slice(2);
  const { scenarioName, bridgeDir, outFile, dryRun, baselineFile } = parseArgs(args);

  const transport: Transport = dryRun ? stubTransport() : fileBridgeTransport(bridgeDir);

  const names: string[] = scenarioName === "all" ? Object.keys(scenarios) : [scenarioName];

  const reports: BenchReport[] = [];

  for (const name of names) {
    const scenario = scenarios[name];
    if (!scenario) {
      console.error(`Unknown scenario: ${name}. Choose from: ${Object.keys(scenarios).join(", ")}`);
      process.exit(1);
    }

    console.log(`\nRunning scenario: ${name}${dryRun ? " (dry-run)" : ""}`);
    const { summary, records } = await runScenario(scenario, transport);
    printSummary(name, summary);

    if (baselineFile) {
      await printBaseline(name, baselineFile, summary);
    }

    reports.push({
      scenario: name,
      summary,
      records,
      timestamp: new Date().toISOString(),
    });
  }

  if (outFile) {
    await Bun.write(outFile, JSON.stringify(reports, null, 2));
    console.log(`\nReport written to: ${outFile}`);
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
