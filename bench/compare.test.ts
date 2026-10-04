/**
 * Unit tests for bench/compare.ts.
 * Run with: bun test bench/compare.test.ts
 */

import { describe, expect, it } from "bun:test";
import { buildComparison, byteRatio } from "./compare.js";
import type { BenchReport } from "./harness.js";

function fakeReport(overrides: Partial<BenchReport>): BenchReport {
  return {
    scenario: "webpage-plain",
    label: "transport",
    target: "turbofig-bridge",
    runs: 10,
    timeoutMs: 45_000,
    valid: true,
    invalidReason: null,
    jobStats: [
      {
        index: 0,
        coldMs: 100,
        warmMedianMs: 50,
        warmP95Ms: 60,
        requestBytes: 200,
        responseBytes: 20,
        failures: 0,
      },
    ],
    totalRequestBytes: 200,
    totalResponseBytes: 20,
    failedIterations: 0,
    iterations: [],
    machine: {},
    timestamp: new Date().toISOString(),
    ...overrides,
  };
}

describe("buildComparison", () => {
  it("groups reports by scenario, one column per target", () => {
    const rows = buildComparison([
      fakeReport({ target: "turbofig-bridge" }),
      fakeReport({ target: "console-mcp", totalRequestBytes: 400 }),
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0].perTarget["turbofig-bridge"]?.totalRequestBytes).toBe(200);
    expect(rows[0].perTarget["console-mcp"]?.totalRequestBytes).toBe(400);
  });

  it("keeps separate scenarios as separate rows", () => {
    const rows = buildComparison([
      fakeReport({ scenario: "webpage-plain" }),
      fakeReport({ scenario: "deck20-plain" }),
    ]);
    expect(rows.map((r) => r.scenario).sort()).toEqual(["deck20-plain", "webpage-plain"]);
  });

  it("sums warmMedianMs across jobs, and returns null if any job has no warm data", () => {
    const withGap = fakeReport({
      jobStats: [
        {
          index: 0,
          coldMs: 10,
          warmMedianMs: 50,
          warmP95Ms: 60,
          requestBytes: 1,
          responseBytes: 1,
          failures: 0,
        },
        {
          index: 1,
          coldMs: 10,
          warmMedianMs: null,
          warmP95Ms: null,
          requestBytes: 1,
          responseBytes: 1,
          failures: 0,
        },
      ],
    });
    const rows = buildComparison([withGap]);
    expect(rows[0].perTarget["turbofig-bridge"]?.warmMedianMsSum).toBeNull();
  });
});

describe("byteRatio", () => {
  it("computes numerator/denominator total bytes", () => {
    const rows = buildComparison([
      fakeReport({ target: "turbofig-bridge", totalRequestBytes: 200, totalResponseBytes: 0 }),
      fakeReport({ target: "console-mcp", totalRequestBytes: 400, totalResponseBytes: 0 }),
    ]);
    expect(byteRatio(rows[0], "turbofig-bridge", "console-mcp")).toBe(0.5);
  });

  it("returns null when the denominator target is missing", () => {
    const rows = buildComparison([fakeReport({ target: "turbofig-bridge" })]);
    expect(byteRatio(rows[0], "turbofig-bridge", "console-mcp")).toBeNull();
  });

  it("returns null when either side is invalid, so a failed run never leaks into the ratio", () => {
    const rows = buildComparison([
      fakeReport({ target: "turbofig-bridge" }),
      fakeReport({ target: "console-mcp", valid: false, invalidReason: "job 1 failed" }),
    ]);
    expect(byteRatio(rows[0], "turbofig-bridge", "console-mcp")).toBeNull();
  });
});
