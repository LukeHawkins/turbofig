/**
 * Unit tests for bench/harness.ts pure functions and run orchestration.
 * Run with: bun test bench/harness.test.ts
 */

import { describe, expect, it } from "bun:test";
import type { BenchReport, IterationRecord } from "./harness.js";
import {
  compareToBaseline,
  median,
  p95,
  runScenarioAgainstTarget,
  summarizeJobs,
} from "./harness.js";
import type { BridgeJob } from "./scenarios.js";
import type { SubmitResult, Transport } from "./transports.js";

// ---------------------------------------------------------------------------
// median / p95
// ---------------------------------------------------------------------------

describe("median", () => {
  it("returns the single value for a one-element array", () => {
    expect(median([42])).toBe(42);
  });

  it("averages the two middle values for an even-length array", () => {
    expect(median([10, 20, 30, 40])).toBe(25);
  });

  it("returns the middle value for an odd-length array", () => {
    expect(median([5, 1, 9])).toBe(5);
  });
});

describe("p95", () => {
  it("returns the single value for a one-element array", () => {
    expect(p95([42])).toBe(42);
  });

  it("returns the highest value when all ranks point past the array", () => {
    expect(p95([1, 2, 3, 4])).toBe(4);
  });

  it("picks the nearest-rank value for a larger array", () => {
    const values = Array.from({ length: 20 }, (_, i) => i + 1); // 1..20
    // ceil(0.95 * 20) - 1 = 18 -> values[18] = 19
    expect(p95(values)).toBe(19);
  });
});

// ---------------------------------------------------------------------------
// summarizeJobs
// ---------------------------------------------------------------------------

function fakeIteration(
  iteration: number,
  jobTimes: number[],
  opts: { bytes?: [number, number][]; allOk?: boolean } = {},
): IterationRecord {
  const records = jobTimes.map((wallMs, index) => ({
    role: "job" as const,
    index,
    ok: opts.allOk ?? true,
    wallMs,
    requestBytes: opts.bytes?.[index]?.[0] ?? 10,
    responseBytes: opts.bytes?.[index]?.[1] ?? 20,
  }));
  return { iteration, cold: iteration === 0, valid: records.every((r) => r.ok), records };
}

describe("summarizeJobs", () => {
  it("takes coldMs from iteration 0 only", () => {
    const iterations = [fakeIteration(0, [100]), fakeIteration(1, [50]), fakeIteration(2, [60])];
    const stats = summarizeJobs(iterations, 1);
    expect(stats[0].coldMs).toBe(100);
  });

  it("computes warm median and p95 from iterations after the first", () => {
    const iterations = [
      fakeIteration(0, [999]),
      fakeIteration(1, [10]),
      fakeIteration(2, [20]),
      fakeIteration(3, [30]),
    ];
    const stats = summarizeJobs(iterations, 1);
    expect(stats[0].warmMedianMs).toBe(20);
    expect(stats[0].warmP95Ms).toBe(30);
  });

  it("returns null cold/warm stats when no iterations are given", () => {
    const stats = summarizeJobs([], 1);
    expect(stats[0].coldMs).toBeNull();
    expect(stats[0].warmMedianMs).toBeNull();
    expect(stats[0].warmP95Ms).toBeNull();
  });

  it("carries request and response bytes from the first iteration with a record", () => {
    const iterations = [fakeIteration(0, [10], { bytes: [[123, 456]] })];
    const stats = summarizeJobs(iterations, 1);
    expect(stats[0].requestBytes).toBe(123);
    expect(stats[0].responseBytes).toBe(456);
  });
});

// ---------------------------------------------------------------------------
// compareToBaseline
// ---------------------------------------------------------------------------

function fakeReport(overrides: Partial<BenchReport> = {}): BenchReport {
  return {
    scenario: "webpage",
    label: "transport + helpers",
    target: "turbofig-bridge",
    runs: 10,
    timeoutMs: 45_000,
    valid: true,
    invalidReason: null,
    jobStats: [],
    totalRequestBytes: 100,
    totalResponseBytes: 50,
    iterations: [],
    machine: {},
    timestamp: new Date().toISOString(),
    ...overrides,
  };
}

describe("compareToBaseline", () => {
  it("returns ratio string when scenario and target match", () => {
    const report = fakeReport({ totalRequestBytes: 60, totalResponseBytes: 40 });
    const baseline = fakeReport({ totalRequestBytes: 80, totalResponseBytes: 20 });
    // this total 100 / baseline total 100 = 1.000
    expect(compareToBaseline(report, baseline)).toBe("1.000");
  });

  it("returns null when scenario does not match", () => {
    const report = fakeReport({ scenario: "deck20" });
    const baseline = fakeReport({ scenario: "webpage" });
    expect(compareToBaseline(report, baseline)).toBeNull();
  });

  it("returns null when target does not match", () => {
    const report = fakeReport({ target: "turbofig-mcp" });
    const baseline = fakeReport({ target: "turbofig-bridge" });
    expect(compareToBaseline(report, baseline)).toBeNull();
  });

  it("returns 'no valid baseline total' when baseline total is zero", () => {
    const report = fakeReport();
    const baseline = fakeReport({ totalRequestBytes: 0, totalResponseBytes: 0 });
    expect(compareToBaseline(report, baseline)).toBe("no valid baseline total");
  });

  it("returns 'no valid baseline total' when baseline total is negative", () => {
    const report = fakeReport();
    const baseline = fakeReport({ totalRequestBytes: -10, totalResponseBytes: 0 });
    expect(compareToBaseline(report, baseline)).toBe("no valid baseline total");
  });
});

// ---------------------------------------------------------------------------
// runScenarioAgainstTarget (mock transport)
// ---------------------------------------------------------------------------

function mockTransport(result: (job: BridgeJob) => SubmitResult): Transport {
  return {
    async submit(job: BridgeJob): Promise<SubmitResult> {
      return result(job);
    },
  };
}

const okResult: SubmitResult = {
  result: { ok: true },
  wallMs: 5,
  requestBytes: 10,
  responseBytes: 10,
  ok: true,
};

describe("runScenarioAgainstTarget", () => {
  const scenario = {
    name: "test",
    label: "transport" as const,
    targets: ["turbofig-bridge" as const],
    pageName: "bench-test",
    jobs: [
      { op: "execute" as const, code: "tf.a()" },
      { op: "execute" as const, code: "tf.b()" },
    ],
  };

  it("runs setup, every job, and teardown each iteration", async () => {
    const transport = mockTransport(() => okResult);
    const { iterations, valid } = await runScenarioAgainstTarget(scenario, transport, 2, undefined);
    expect(valid).toBe(true);
    expect(iterations).toHaveLength(2);
    for (const iter of iterations) {
      expect(iter.records.map((r) => r.role)).toEqual(["setup", "job", "job", "teardown"]);
    }
  });

  it("marks the run invalid and records the reason when a job fails", async () => {
    let call = 0;
    const transport = mockTransport((job) => {
      call++;
      if (job.code === "tf.b()") {
        return {
          result: { ok: false, error: "boom" },
          wallMs: 1,
          requestBytes: 1,
          responseBytes: 1,
          ok: false,
        };
      }
      return okResult;
    });
    const { valid, invalidReason } = await runScenarioAgainstTarget(
      scenario,
      transport,
      1,
      undefined,
    );
    expect(valid).toBe(false);
    expect(invalidReason).toContain("job 1 failed");
    expect(call).toBeGreaterThan(0);
  });

  it("skips the scenario jobs and teardown when setup fails, but does not throw", async () => {
    const transport = mockTransport((job) => {
      if (job.code?.includes("createPage")) {
        return {
          result: { ok: false, error: "no plugin" },
          wallMs: 1,
          requestBytes: 1,
          responseBytes: 1,
          ok: false,
        };
      }
      return okResult;
    });
    const { iterations, valid } = await runScenarioAgainstTarget(scenario, transport, 1, undefined);
    expect(valid).toBe(false);
    expect(iterations[0].records).toHaveLength(1);
    expect(iterations[0].records[0].role).toBe("setup");
  });

  it("records a transport exception as a failed job instead of throwing", async () => {
    const transport: Transport = {
      async submit(): Promise<SubmitResult> {
        throw new Error("network down");
      },
    };
    const { valid, invalidReason } = await runScenarioAgainstTarget(
      scenario,
      transport,
      1,
      undefined,
    );
    expect(valid).toBe(false);
    expect(invalidReason).toContain("network down");
  });

  it("passes fileKey through to every job when given", async () => {
    const seenKeys: (string | undefined)[] = [];
    const transport = mockTransport((job) => {
      seenKeys.push(job.fileKey);
      return okResult;
    });
    await runScenarioAgainstTarget(scenario, transport, 1, "abc123");
    expect(seenKeys.every((k) => k === "abc123")).toBe(true);
  });
});
