import { describe, expect, test } from "bun:test";
import { backoffDelayMs, isDaemonMessage } from "./protocol";

describe("isDaemonMessage", () => {
  test("accepts a valid FILE_INFO message", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", fileKey: "abc123", name: "My File" })).toBe(true);
  });

  test("accepts a valid STATUS message", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: 1 })).toBe(true);
  });

  test("accepts a valid RESULT message", () => {
    expect(isDaemonMessage({ type: "RESULT", requestId: 1 })).toBe(true);
  });

  test("accepts a RESULT message with extra fields", () => {
    expect(
      isDaemonMessage({ type: "RESULT", requestId: 2, ok: true, fileKey: "f", name: "n" }),
    ).toBe(true);
  });

  test("rejects null", () => {
    expect(isDaemonMessage(null)).toBe(false);
  });

  test("rejects a plain string", () => {
    expect(isDaemonMessage("FILE_INFO")).toBe(false);
  });

  test("rejects an unknown type", () => {
    expect(isDaemonMessage({ type: "EXECUTE" })).toBe(false);
  });

  test("rejects FILE_INFO missing fileKey", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", name: "My File" })).toBe(false);
  });

  test("rejects FILE_INFO missing name", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", fileKey: "abc123" })).toBe(false);
  });

  test("rejects STATUS with a string requestId", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: "req-1" })).toBe(false);
  });

  test("rejects STATUS missing requestId", () => {
    expect(isDaemonMessage({ type: "STATUS" })).toBe(false);
  });

  test("rejects an empty object", () => {
    expect(isDaemonMessage({})).toBe(false);
  });
});

describe("backoffDelayMs", () => {
  test("attempt 0 returns the base delay of 500ms", () => {
    expect(backoffDelayMs(0)).toBe(500);
  });

  test("attempt 1 returns 1000ms (doubles each step)", () => {
    expect(backoffDelayMs(1)).toBe(1000);
  });

  test("attempt 2 returns 2000ms", () => {
    expect(backoffDelayMs(2)).toBe(2000);
  });

  test("grows monotonically until the cap", () => {
    let prev = backoffDelayMs(0);
    for (let i = 1; i <= 6; i++) {
      const curr = backoffDelayMs(i);
      expect(curr).toBeGreaterThanOrEqual(prev);
      prev = curr;
    }
  });

  test("caps at 30000ms for attempt 6 and beyond", () => {
    // 500 * 2^6 = 32000 > 30000, so attempt 6 must be capped
    expect(backoffDelayMs(6)).toBe(30000);
    expect(backoffDelayMs(100)).toBe(30000);
  });

  test("never exceeds the 30000ms cap", () => {
    for (let i = 0; i <= 50; i++) {
      expect(backoffDelayMs(i)).toBeLessThanOrEqual(30000);
    }
  });
});
