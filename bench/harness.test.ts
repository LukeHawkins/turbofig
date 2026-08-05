/**
 * Unit tests for bench/harness.ts pure functions.
 * Run with: bun test bench/harness.test.ts
 */

import { describe, expect, it } from "bun:test";
import { estimateTokens, jobTokens, type RunRecord, runScenario, summarizeRun } from "./harness.js";
import type { BridgeJob, Scenario } from "./scenarios.js";

// ---------------------------------------------------------------------------
// estimateTokens
// ---------------------------------------------------------------------------

describe("estimateTokens", () => {
  it("returns 0 for empty string", () => {
    expect(estimateTokens("")).toBe(0);
  });

  it("returns 1 for 4 characters", () => {
    expect(estimateTokens("abcd")).toBe(1);
  });

  it("rounds up when characters do not divide evenly by 4", () => {
    // 5 chars => ceil(5/4) = 2
    expect(estimateTokens("abcde")).toBe(2);
  });

  it("returns 1 for 1 to 4 characters", () => {
    // ceil(1/4) = 1, ceil(3/4) = 1
    expect(estimateTokens("a")).toBe(1);
    expect(estimateTokens("abc")).toBe(1);
  });

  it("returns exact integer when chars divide evenly", () => {
    // 12 chars => ceil(12/4) = 3
    expect(estimateTokens("abcdefghijkl")).toBe(3);
  });
});

// ---------------------------------------------------------------------------
// jobTokens
// ---------------------------------------------------------------------------

describe("jobTokens", () => {
  it("counts tokens from the JSON-serialised job", () => {
    const job: BridgeJob = { op: "execute", code: "tf.noop()" };
    // JSON.stringify => '{"op":"execute","code":"tf.noop()"}' = 35 chars => ceil(35/4) = 9
    const serialised = JSON.stringify(job);
    expect(jobTokens(job)).toBe(Math.ceil(serialised.length / 4));
  });

  it("handles a status job with no code field", () => {
    const job: BridgeJob = { op: "status" };
    const serialised = JSON.stringify(job);
    expect(jobTokens(job)).toBe(Math.ceil(serialised.length / 4));
  });

  it("includes fileKey when present", () => {
    const jobWithKey: BridgeJob = { op: "screenshot", fileKey: "abc123" };
    const jobWithout: BridgeJob = { op: "screenshot" };
    // Job with key serialises to more chars.
    expect(jobTokens(jobWithKey)).toBeGreaterThan(jobTokens(jobWithout));
  });
});

// ---------------------------------------------------------------------------
// summarizeRun
// ---------------------------------------------------------------------------

describe("summarizeRun", () => {
  it("returns zeroed summary for empty records", () => {
    const summary = summarizeRun([]);
    expect(summary.tokensIn).toBe(0);
    expect(summary.tokensOut).toBe(0);
    expect(summary.totalTokens).toBe(0);
    expect(summary.wallMs).toBe(0);
    expect(summary.count).toBe(0);
  });

  it("sums all fields across records", () => {
    const records: RunRecord[] = [
      { tokensIn: 10, tokensOut: 5, wallMs: 100 },
      { tokensIn: 20, tokensOut: 8, wallMs: 200 },
    ];
    const summary = summarizeRun(records);
    expect(summary.tokensIn).toBe(30);
    expect(summary.tokensOut).toBe(13);
    expect(summary.totalTokens).toBe(43);
    expect(summary.wallMs).toBe(300);
    expect(summary.count).toBe(2);
  });

  it("sets totalTokens to tokensIn + tokensOut", () => {
    const records: RunRecord[] = [{ tokensIn: 7, tokensOut: 3, wallMs: 50 }];
    const summary = summarizeRun(records);
    expect(summary.totalTokens).toBe(summary.tokensIn + summary.tokensOut);
  });
});

// ---------------------------------------------------------------------------
// runScenario with stub transport
// ---------------------------------------------------------------------------

describe("runScenario", () => {
  it("returns count matching number of jobs in scenario", async () => {
    const fixedResult = { ok: true };
    const stub = {
      async submit(_job: BridgeJob) {
        return { result: fixedResult, wallMs: 0 };
      },
    };

    const twoJobScenario: Scenario = {
      name: "test",
      jobs: [
        { op: "execute", code: "tf.a()" },
        { op: "execute", code: "tf.b()" },
      ],
    };

    const { summary, records } = await runScenario(twoJobScenario, stub);
    expect(summary.count).toBe(2);
    expect(records.length).toBe(2);
  });

  it("accumulates tokensIn from each job and tokensOut from each result", async () => {
    const fixedResult = { ok: true };
    const stub = {
      async submit(_job: BridgeJob) {
        return { result: fixedResult, wallMs: 0 };
      },
    };

    const job1: BridgeJob = { op: "execute", code: "tf.alpha()" };
    const job2: BridgeJob = { op: "execute", code: "tf.beta()" };

    const scenario: Scenario = { name: "test2", jobs: [job1, job2] };

    const { summary } = await runScenario(scenario, stub);

    const expectedIn = jobTokens(job1) + jobTokens(job2);
    const expectedOut = estimateTokens(JSON.stringify(fixedResult)) * 2;

    expect(summary.tokensIn).toBe(expectedIn);
    expect(summary.tokensOut).toBe(expectedOut);
    expect(summary.totalTokens).toBe(expectedIn + expectedOut);
  });

  it("records wallMs from the stub transport per job", async () => {
    const stub = {
      async submit(_job: BridgeJob) {
        return { result: { ok: true }, wallMs: 42 };
      },
    };

    const scenario: Scenario = {
      name: "timing",
      jobs: [{ op: "status" }, { op: "status" }],
    };

    const { summary, records } = await runScenario(scenario, stub);
    expect(records[0].wallMs).toBe(42);
    expect(records[1].wallMs).toBe(42);
    expect(summary.wallMs).toBe(84);
  });
});
