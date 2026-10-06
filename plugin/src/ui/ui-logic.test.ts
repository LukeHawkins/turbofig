import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  appendLog,
  type ConnectedFile,
  connStateFromEvent,
  daemonMessageAction,
  fillAgentPrompt,
  formatLogEntry,
  formatSession,
  isDaemonStale,
  isRequestType,
  mainMessageAction,
  parsePort,
  portMessageAction,
  screenSize,
  staleWarning,
  wsUrlForPort,
} from "./ui-logic";

/**
 * The real shared template (`prompts/agent-prompt.txt`), read straight from
 * disk rather than duplicated here: `fillAgentPrompt`'s tests below exercise
 * the exact same file `build-ui.ts` inlines into the plugin bundle and
 * `daemon/src/agent_prompt.rs` embeds with `include_str!`.
 */
const AGENT_PROMPT_TEMPLATE = readFileSync(
  join(import.meta.dir, "../../../prompts/agent-prompt.txt"),
  "utf8",
);

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

describe("screenSize", () => {
  test("main screen returns 300x150", () => {
    const { width, height } = screenSize("main");
    expect(width).toBe(300);
    expect(height).toBe(150);
  });

  test("advanced screen returns 300x240", () => {
    const { width, height } = screenSize("advanced");
    expect(width).toBe(300);
    expect(height).toBe(240);
  });

  test("main and advanced return different heights", () => {
    expect(screenSize("main").height).not.toBe(screenSize("advanced").height);
  });

  test("both screens return the same width (300)", () => {
    expect(screenSize("main").width).toBe(screenSize("advanced").width);
  });

  test("about screen returns 300x375", () => {
    const { width, height } = screenSize("about");
    expect(width).toBe(300);
    expect(height).toBe(375);
  });

  test("about screen height differs from main and advanced", () => {
    expect(screenSize("about").height).not.toBe(screenSize("main").height);
    expect(screenSize("about").height).not.toBe(screenSize("advanced").height);
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
    expect(wsUrlForPort(18847, "tok")).toBe("ws://127.0.0.1:18847?token=tok");
  });

  test("returns a URL for a custom port", () => {
    expect(wsUrlForPort(8080, "tok")).toBe("ws://127.0.0.1:8080?token=tok");
  });

  test("returns a URL for port 1", () => {
    expect(wsUrlForPort(1, "tok")).toBe("ws://127.0.0.1:1?token=tok");
  });

  test("returns a URL for port 65535", () => {
    expect(wsUrlForPort(65535, "tok")).toBe("ws://127.0.0.1:65535?token=tok");
  });

  test("URL-encodes a token with special characters", () => {
    expect(wsUrlForPort(18847, "a b&c")).toBe("ws://127.0.0.1:18847?token=a%20b%26c");
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
    expect(isRequestType("RESULT")).toBe(false);
    expect(isRequestType("")).toBe(false);
  });
});

describe("daemonMessageAction", () => {
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
    expect(mainMessageAction("SET_PORT")).toBe("relay");
  });
});

describe("portMessageAction", () => {
  test("the first PORT ever received connects, regardless of the port values", () => {
    expect(portMessageAction(false, 18847, 18847)).toBe("connect");
    expect(portMessageAction(false, 18847, 9000)).toBe("connect");
  });

  test("a later PORT naming a different port reconnects", () => {
    expect(portMessageAction(true, 18847, 9000)).toBe("reconnect");
  });

  test("a later PORT naming the same port does nothing", () => {
    expect(portMessageAction(true, 18847, 18847)).toBe("none");
  });
});

describe("fillAgentPrompt", () => {
  const file = (fileKey: string, name: string): ConnectedFile => ({ fileKey, name });

  test("0 files: fileKey placeholder, tells the agent to run status first", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "/tmp/custom-bridge", 18846);
    expect(result).toContain('"fileKey":"<fileKey>"');
    expect(result).toContain("Run the status op first to learn the fileKey.");
    expect(result).not.toContain("Connected files:");
  });

  test("exactly 1 file: fills its fileKey, no extra hint", () => {
    const result = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      [file("ABC123fileKey", "Design A")],
      "/tmp/custom-bridge",
      18846,
    );
    expect(result).toContain('"fileKey":"ABC123fileKey"');
    expect(result).not.toContain("Connected files:");
    expect(result).not.toContain("Run the status op first");
  });

  test("more than 1 file: lists every name and fileKey, keeps the placeholder", () => {
    const result = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      [file("key1", "Design A"), file("key2", "Design B")],
      "/tmp/custom-bridge",
      18846,
    );
    expect(result).toContain("Connected files: Design A (key1), Design B (key2).");
    expect(result).toContain('"fileKey":"<fileKey>"');
    expect(result).toContain("Pick a fileKey from the list above.");
  });

  test("leads with the file-bridge inbox and outbox paths", () => {
    const result = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      [file("someKey", "Design A")],
      "/tmp/custom-bridge",
      18846,
    );
    expect(result).toContain("/tmp/custom-bridge/inbox/");
    expect(result).toContain("/tmp/custom-bridge/outbox/");
  });

  test("mentions MCP only as a fallback, with the correct port and path", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "/tmp/custom-bridge", 19999);
    expect(result).toContain("http://127.0.0.1:19999/mcp");
    expect(result).not.toContain("18846");
  });

  test("never instructs a bare GET or a web-fetch (which forces HTTPS and breaks)", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "/tmp/custom-bridge", 18846);
    expect(result).not.toContain("GET ");
    expect(result).toContain("curl");
    expect(result).toContain("web-fetch");
  });

  test("puts the file-bridge before the MCP fallback", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "/tmp/custom-bridge", 18846);
    expect(result.indexOf("inbox")).toBeLessThan(result.indexOf("/mcp"));
  });

  test("falls back to ~/.turbofig when bridgeDir is empty", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "", 18846);
    expect(result).toContain("~/.turbofig/inbox/");
    expect(result).toContain("~/.turbofig/outbox/");
  });

  test("tells the agent to run turbofig start and retry if no result appears", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "/tmp/custom-bridge", 18846);
    expect(result).toContain("turbofig start");
    expect(result).toContain("retry");
  });

  /**
   * Golden test: a fixed set of inputs must always produce this exact text.
   * `daemon/src/agent_prompt.rs` has a matching test with the same inputs
   * and the same expected string, so both implementations are checked
   * against one shared fixture rather than against each other directly.
   */
  test("golden: matches the fixed expected text for each file count", () => {
    const zero = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, [], "/tmp/bridge", 18846);
    expect(zero).toBe(
      'turbofig file-bridge: write {"op":"execute","fileKey":"<fileKey>","code":"..."} to /tmp/bridge/inbox/<unique-id>.json (id unique per job) -> read /tmp/bridge/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. Run the status op first to learn the fileKey. If no result file appears within a few seconds, run `turbofig start` once, then retry. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:18846/mcp\n',
    );

    const one = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      [file("ABC123", "Design A")],
      "/tmp/bridge",
      18846,
    );
    expect(one).toBe(
      'turbofig file-bridge: write {"op":"execute","fileKey":"ABC123","code":"..."} to /tmp/bridge/inbox/<unique-id>.json (id unique per job) -> read /tmp/bridge/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. If no result file appears within a few seconds, run `turbofig start` once, then retry. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:18846/mcp\n',
    );

    const many = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      [file("key1", "Design A"), file("key2", "Design B")],
      "/tmp/bridge",
      18846,
    );
    expect(many).toBe(
      'Connected files: Design A (key1), Design B (key2).\nturbofig file-bridge: write {"op":"execute","fileKey":"<fileKey>","code":"..."} to /tmp/bridge/inbox/<unique-id>.json (id unique per job) -> read /tmp/bridge/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. Pick a fileKey from the list above. If no result file appears within a few seconds, run `turbofig start` once, then retry. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:18846/mcp\n',
    );
  });
});
