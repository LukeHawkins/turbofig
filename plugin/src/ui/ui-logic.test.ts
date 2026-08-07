import { describe, expect, test } from "bun:test";
import {
  appendLog,
  connStateFromEvent,
  daemonMessageAction,
  formatConnectPrompt,
  formatFileLine,
  formatLogEntry,
  formatSession,
  isDaemonStale,
  isRequestType,
  KNOWN_PROFILES,
  mainMessageAction,
  parsePort,
  profileToSelectValue,
  screenSize,
  staleWarning,
  wsUrlForPort,
} from "./ui-logic";

describe("connStateFromEvent", () => {
  test("open event returns connected state", () => {
    const r = connStateFromEvent("open", 0);
    expect(r.state).toBe("connected");
    expect(r.label).toBe("Connected");
  });

  test("open event returns connected regardless of attempt count", () => {
    const r = connStateFromEvent("open", 10);
    expect(r.state).toBe("connected");
  });

  test("attempt at 0 returns connecting state", () => {
    const r = connStateFromEvent("attempt", 0);
    expect(r.state).toBe("connecting");
  });

  test("attempt at 1 returns reconnecting state", () => {
    const r = connStateFromEvent("attempt", 1);
    expect(r.state).toBe("reconnecting");
  });

  test("attempt at 4 returns reconnecting state", () => {
    const r = connStateFromEvent("attempt", 4);
    expect(r.state).toBe("reconnecting");
  });

  test("attempt at 5 returns offline state", () => {
    const r = connStateFromEvent("attempt", 5);
    expect(r.state).toBe("offline");
    expect(r.label).toBe("Offline");
  });

  test("attempt at 10 returns offline state", () => {
    const r = connStateFromEvent("attempt", 10);
    expect(r.state).toBe("offline");
  });

  test("close at 0 returns connecting state", () => {
    const r = connStateFromEvent("close", 0);
    expect(r.state).toBe("connecting");
  });

  test("close at 3 returns reconnecting state", () => {
    const r = connStateFromEvent("close", 3);
    expect(r.state).toBe("reconnecting");
  });

  test("close at 5 returns offline state", () => {
    const r = connStateFromEvent("close", 5);
    expect(r.state).toBe("offline");
  });

  test("error event returns reconnecting state", () => {
    const r = connStateFromEvent("error", 0);
    expect(r.state).toBe("reconnecting");
  });

  test("error event with high attempt count still returns reconnecting", () => {
    const r = connStateFromEvent("error", 10);
    expect(r.state).toBe("reconnecting");
  });

  test("all events return a non-empty label", () => {
    const cases: Array<[Parameters<typeof connStateFromEvent>[0], number]> = [
      ["open", 0],
      ["attempt", 0],
      ["attempt", 3],
      ["attempt", 5],
      ["close", 1],
      ["error", 0],
    ];
    for (const [event, attempt] of cases) {
      const r = connStateFromEvent(event, attempt);
      expect(r.label.length).toBeGreaterThan(0);
    }
  });
});

describe("formatFileLine", () => {
  test("returns name with shortened key for full inputs", () => {
    const result = formatFileLine("ABCDEF1234567890", "My Design");
    expect(result).toBe("My Design (ABCDEF12)");
  });

  test("returns truncated key when name is empty", () => {
    const result = formatFileLine("ABCDEF12EXTRA", "");
    expect(result).toBe("ABCDEF12");
  });

  test("returns just the name when fileKey is empty", () => {
    const result = formatFileLine("", "My Design");
    expect(result).toBe("My Design");
  });

  test("returns No file when both are empty", () => {
    const result = formatFileLine("", "");
    expect(result).toBe("No file");
  });

  test("handles a short key without truncation", () => {
    const result = formatFileLine("ABC", "File");
    expect(result).toBe("File (ABC)");
  });

  test("key exactly 8 characters is not truncated", () => {
    const result = formatFileLine("12345678", "Doc");
    expect(result).toBe("Doc (12345678)");
  });
});

describe("formatSession", () => {
  test("returns No active session when empty", () => {
    expect(formatSession("")).toBe("No active session");
  });

  test("returns short id unchanged", () => {
    expect(formatSession("abc123")).toBe("abc123");
  });

  test("returns id unchanged at exactly 12 characters", () => {
    expect(formatSession("123456789012")).toBe("123456789012");
  });

  test("truncates id at 13 characters with ...", () => {
    expect(formatSession("1234567890123")).toBe("123456789012...");
  });

  test("truncates long id with ...", () => {
    expect(formatSession("abcdefghijklmnopq")).toBe("abcdefghijkl...");
  });
});

describe("profileToSelectValue", () => {
  test("known profiles return their id as value with isCustom false", () => {
    for (const id of KNOWN_PROFILES) {
      const r = profileToSelectValue(id);
      expect(r.value).toBe(id);
      expect(r.isCustom).toBe(false);
    }
  });

  test("unknown profile id returns custom with isCustom true", () => {
    const r = profileToSelectValue("my-brand");
    expect(r.value).toBe("custom");
    expect(r.isCustom).toBe(true);
  });

  test("empty string is not a known id and returns custom", () => {
    const r = profileToSelectValue("");
    expect(r.value).toBe("custom");
    expect(r.isCustom).toBe(true);
  });

  test("impeccable is a known profile", () => {
    const r = profileToSelectValue("impeccable");
    expect(r.isCustom).toBe(false);
  });

  test("minimal is a known profile", () => {
    const r = profileToSelectValue("minimal");
    expect(r.isCustom).toBe(false);
  });
});

describe("screenSize", () => {
  test("main screen returns 300x200", () => {
    const { width, height } = screenSize("main");
    expect(width).toBe(300);
    expect(height).toBe(200);
  });

  test("advanced screen returns 300x440", () => {
    const { width, height } = screenSize("advanced");
    expect(width).toBe(300);
    expect(height).toBe(440);
  });

  test("main and advanced return different heights", () => {
    expect(screenSize("main").height).not.toBe(screenSize("advanced").height);
  });

  test("both screens return the same width (300)", () => {
    expect(screenSize("main").width).toBe(screenSize("advanced").width);
  });
});

describe("appendLog", () => {
  test("appends an entry to an empty log", () => {
    const result = appendLog([], "hello", 10);
    expect(result).toEqual(["hello"]);
  });

  test("appends to an existing log without mutating the input", () => {
    const original = ["a", "b"];
    const result = appendLog(original, "c", 10);
    expect(result).toEqual(["a", "b", "c"]);
    expect(original).toEqual(["a", "b"]);
  });

  test("does not mutate the input array", () => {
    const original = ["x"];
    appendLog(original, "y", 5);
    expect(original).toHaveLength(1);
  });

  test("trims to the last cap items when the log exceeds cap", () => {
    const log = ["a", "b", "c", "d", "e"];
    const result = appendLog(log, "f", 3);
    expect(result).toEqual(["d", "e", "f"]);
  });

  test("returns the full log when its length is below cap", () => {
    const log = ["a", "b"];
    const result = appendLog(log, "c", 10);
    expect(result).toHaveLength(3);
  });

  test("cap of 1 keeps only the newest entry", () => {
    const result = appendLog(["old"], "new", 1);
    expect(result).toEqual(["new"]);
  });

  test("works with number entries", () => {
    const result = appendLog([1, 2, 3], 4, 3);
    expect(result).toEqual([2, 3, 4]);
  });
});

describe("formatLogEntry", () => {
  test("includes the request type in the output", () => {
    const result = formatLogEntry("EXECUTE", 0);
    expect(result).toContain("EXECUTE");
  });

  test("is deterministic for a fixed timestamp", () => {
    const ts = 1700000000000;
    expect(formatLogEntry("STATUS", ts)).toBe(formatLogEntry("STATUS", ts));
  });

  test("formats midnight UTC as 00:00:00", () => {
    // 2024-01-01T00:00:00.000Z
    const ts = new Date("2024-01-01T00:00:00.000Z").getTime();
    const result = formatLogEntry("STATUS", ts);
    expect(result.startsWith("00:00:00")).toBe(true);
  });

  test("formats 14:05:02 UTC correctly", () => {
    const ts = new Date("2024-06-15T14:05:02.000Z").getTime();
    const result = formatLogEntry("EXECUTE", ts);
    expect(result).toBe("14:05:02 EXECUTE");
  });

  test("includes leading zeros for single-digit hours and minutes", () => {
    const ts = new Date("2024-01-01T01:02:03.000Z").getTime();
    const result = formatLogEntry("SCREENSHOT", ts);
    expect(result).toBe("01:02:03 SCREENSHOT");
  });
});

describe("isDaemonStale", () => {
  test("equal versions are not stale", () => {
    expect(isDaemonStale("0.1.0", "0.1.0")).toBe(false);
  });

  test("differing versions are stale", () => {
    expect(isDaemonStale("0.1.0", "0.2.0")).toBe(true);
  });

  test("empty plugin version is not stale", () => {
    expect(isDaemonStale("", "0.1.0")).toBe(false);
  });

  test("empty daemon version is not stale", () => {
    expect(isDaemonStale("0.1.0", "")).toBe(false);
  });

  test("both empty is not stale", () => {
    expect(isDaemonStale("", "")).toBe(false);
  });

  test("patch version difference is stale", () => {
    expect(isDaemonStale("0.1.0", "0.1.1")).toBe(true);
  });
});

describe("staleWarning", () => {
  test("returns empty string when versions are equal", () => {
    expect(staleWarning("0.1.0", "0.1.0")).toBe("");
  });

  test("returns empty string when plugin version is empty", () => {
    expect(staleWarning("", "0.1.0")).toBe("");
  });

  test("returns empty string when daemon version is empty", () => {
    expect(staleWarning("0.1.0", "")).toBe("");
  });

  test("returns a non-empty string when versions differ", () => {
    expect(staleWarning("0.1.0", "0.2.0").length).toBeGreaterThan(0);
  });

  test("warning includes both version strings", () => {
    const w = staleWarning("0.1.0", "0.2.0");
    expect(w).toContain("0.1.0");
    expect(w).toContain("0.2.0");
  });
});

describe("parsePort", () => {
  test("returns a valid mid-range port", () => {
    expect(parsePort("8080")).toBe(8080);
  });

  test("accepts the minimum valid port (1)", () => {
    expect(parsePort("1")).toBe(1);
  });

  test("accepts the maximum valid port (65535)", () => {
    expect(parsePort("65535")).toBe(65535);
  });

  test("accepts the default daemon port (18847)", () => {
    expect(parsePort("18847")).toBe(18847);
  });

  test("returns null for port 0", () => {
    expect(parsePort("0")).toBeNull();
  });

  test("returns null for port 65536", () => {
    expect(parsePort("65536")).toBeNull();
  });

  test("returns null for a non-numeric string", () => {
    expect(parsePort("abc")).toBeNull();
  });

  test("returns null for an empty string", () => {
    expect(parsePort("")).toBeNull();
  });

  test("returns null for a whitespace-only string", () => {
    expect(parsePort("   ")).toBeNull();
  });

  test("returns null for a fractional number", () => {
    expect(parsePort("8080.5")).toBeNull();
  });

  test("accepts a port with surrounding whitespace", () => {
    expect(parsePort("  3000  ")).toBe(3000);
  });
});

describe("wsUrlForPort", () => {
  test("returns the default daemon URL for port 18847", () => {
    expect(wsUrlForPort(18847)).toBe("ws://localhost:18847");
  });

  test("returns a URL for a custom port", () => {
    expect(wsUrlForPort(8080)).toBe("ws://localhost:8080");
  });

  test("returns a URL for port 1", () => {
    expect(wsUrlForPort(1)).toBe("ws://localhost:1");
  });

  test("returns a URL for port 65535", () => {
    expect(wsUrlForPort(65535)).toBe("ws://localhost:65535");
  });
});

describe("isRequestType", () => {
  test("accepts the four daemon request types", () => {
    expect(isRequestType("STATUS")).toBe(true);
    expect(isRequestType("EXECUTE")).toBe(true);
    expect(isRequestType("GET_SELECTION")).toBe(true);
    expect(isRequestType("SCREENSHOT")).toBe(true);
  });

  test("rejects non-request types", () => {
    expect(isRequestType("WELCOME")).toBe(false);
    expect(isRequestType("SET_PROFILE")).toBe(false);
    expect(isRequestType("RESULT")).toBe(false);
    expect(isRequestType("")).toBe(false);
  });
});

describe("daemonMessageAction", () => {
  test("drops SET_PROFILE so the daemon cannot drive the profile", () => {
    expect(daemonMessageAction("SET_PROFILE")).toBe("drop");
  });

  test("consumes WELCOME locally", () => {
    expect(daemonMessageAction("WELCOME")).toBe("welcome");
  });

  test("relays request messages to the main thread", () => {
    expect(daemonMessageAction("STATUS")).toBe("relay");
    expect(daemonMessageAction("EXECUTE")).toBe("relay");
    expect(daemonMessageAction("RESULT")).toBe("relay");
  });
});

describe("mainMessageAction", () => {
  test("consumes PORT locally", () => {
    expect(mainMessageAction("PORT")).toBe("port");
  });

  test("consumes FILE_INFO locally", () => {
    expect(mainMessageAction("FILE_INFO")).toBe("fileinfo");
  });

  test("relays other main-thread messages over the socket", () => {
    expect(mainMessageAction("RESULT")).toBe("relay");
    expect(mainMessageAction("SET_PROFILE")).toBe("relay");
  });
});

describe("formatConnectPrompt", () => {
  test("returns empty string when fileKey is empty", () => {
    expect(formatConnectPrompt("", 18846)).toBe("");
  });

  test("output contains the fileKey", () => {
    const result = formatConnectPrompt("ABC123fileKey", 18846);
    expect(result).toContain("ABC123fileKey");
  });

  test("leads with the file-bridge inbox and outbox paths", () => {
    const result = formatConnectPrompt("someKey", 18846);
    expect(result).toContain("~/.turbofig/inbox/");
    expect(result).toContain("~/.turbofig/outbox/");
  });

  test("gives a concrete execute example carrying the fileKey", () => {
    const result = formatConnectPrompt("ABC123fileKey", 18846);
    expect(result).toContain('"op":"execute"');
    expect(result).toContain('"fileKey":"ABC123fileKey"');
  });

  test("mentions MCP only as a fallback, with the correct port and path", () => {
    const result = formatConnectPrompt("someKey", 19999);
    expect(result).toContain("http://127.0.0.1:19999/mcp");
  });

  test("does not contain the default port when a different port is passed", () => {
    const result = formatConnectPrompt("someKey", 19999);
    expect(result).not.toContain("18846");
  });

  test("never instructs a bare GET or a web-fetch (which forces HTTPS and breaks)", () => {
    const result = formatConnectPrompt("someKey", 18846);
    expect(result).not.toContain("GET ");
    expect(result).toContain("curl");
    expect(result).toContain("web-fetch");
  });

  test("puts the file-bridge before the MCP fallback", () => {
    const result = formatConnectPrompt("someKey", 18846);
    expect(result.indexOf("inbox")).toBeLessThan(result.indexOf("/mcp"));
  });
});
