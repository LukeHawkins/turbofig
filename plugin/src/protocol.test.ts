import { describe, expect, test } from "bun:test";
import { isDaemonMessage } from "./protocol";

describe("isDaemonMessage", () => {
  test("accepts a valid FILE_INFO message", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", fileKey: "abc123", name: "My File" })).toBe(true);
  });

  test("accepts a valid STATUS message", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: "req-1" })).toBe(true);
  });

  test("accepts a valid RESULT message", () => {
    expect(isDaemonMessage({ type: "RESULT", requestId: "req-1" })).toBe(true);
  });

  test("accepts a RESULT message with extra fields", () => {
    expect(isDaemonMessage({ type: "RESULT", requestId: "req-2", data: { ok: true } })).toBe(true);
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

  test("rejects STATUS missing requestId", () => {
    expect(isDaemonMessage({ type: "STATUS" })).toBe(false);
  });

  test("rejects an empty object", () => {
    expect(isDaemonMessage({})).toBe(false);
  });
});
