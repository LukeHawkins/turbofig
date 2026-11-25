import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { buildSetConnectionMode } from "../protocol";
import {
  ADV_HEIGHT,
  ADV_HEIGHT_LOG,
  appendLog,
  buildSocketUrl,
  connStateFromEvent,
  DAEMON_OLD_LABEL,
  daemonMessageAction,
  decideFrame,
  fillAgentPrompt,
  formatLogEntry,
  formatSession,
  generateNonce,
  handleChallenge,
  hasPairingToken,
  healthAfterClose,
  INITIAL_LINK_HEALTH,
  isDaemonStale,
  isRequestType,
  mainMessageAction,
  modeAfterDaemonFrame,
  NO_TOKEN_LABEL,
  parseConnectionMode,
  parsePort,
  parseShowLog,
  portMessageAction,
  promptTemplateFor,
  screenSize,
  shouldRenderLog,
  staleWarning,
  statusOverride,
  trackActiveJobs,
  VERIFY_FAILED_LABEL,
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

/** The MCP variant of the shared prompt (`prompts/agent-prompt-mcp.txt`). */
const MCP_TEMPLATE = readFileSync(
  join(import.meta.dir, "../../../prompts/agent-prompt-mcp.txt"),
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

  test("advanced screen uses ADV_HEIGHT_LOG, or ADV_HEIGHT with the log hidden", () => {
    expect(screenSize("advanced")).toEqual({ width: 300, height: ADV_HEIGHT_LOG });
    expect(screenSize("advanced", true)).toEqual({ width: 300, height: ADV_HEIGHT_LOG });
    expect(screenSize("advanced", false)).toEqual({ width: 300, height: ADV_HEIGHT });
    expect(screenSize("main", false).height).toBe(150);
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

  test("warning names the daemon version", () => {
    const w = staleWarning("0.1.0", "0.2.0");
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

const TOK = "a".repeat(64);
const NP = "00112233445566778899aabbccddeeff";
const ND = "ffeeddccbbaa99887766554433221100";
const PORT = 18847;
/* HMAC(TOK, "turbofig-ws-daemon:" + NP + ":18847"); the daemon test pins the same vector. */
const DAEMON_PROOF = "5b72a3c171af02ca4487efa9391d74c82c8d2db1dad0f7211ac6ca677978a3d6";

describe("buildSocketUrl", () => {
  test("puts the nonce in the URL and no token", () => {
    const url = buildSocketUrl(18847, NP);
    expect(url).toBe(`ws://127.0.0.1:18847/?nonce=${NP}`);
    expect(url).not.toContain("token");
  });

  test("uses a custom port", () => {
    expect(buildSocketUrl(8080, NP)).toBe(`ws://127.0.0.1:8080/?nonce=${NP}`);
  });
});

describe("generateNonce", () => {
  test("returns 32 lowercase hex chars from 16 bytes", () => {
    const n = generateNonce((b) => b.fill(0xab));
    expect(n).toBe("ab".repeat(16));
    expect(n).toMatch(/^[0-9a-f]{32}$/);
  });

  test("differs between calls with real randomness", () => {
    const r = (b: Uint8Array) => crypto.getRandomValues(b);
    expect(generateNonce(r)).not.toBe(generateNonce(r));
  });
});

describe("handleChallenge", () => {
  const good = { type: "CHALLENGE", nonce: NP, proof: DAEMON_PROOF };

  test("accepts a valid proof and returns the plugin AUTH frame", () => {
    const r = handleChallenge({ ...good, nonce: NP }, NP, TOK, PORT);
    expect(r).toEqual({
      ok: true,
      auth: {
        type: "AUTH",
        proof: "ccee347e4ca98ab0341bfb3b391044a3c10cb9fd3b7a0763709abab5042e6fa3",
      },
    });
  });

  test("rejects a wrong proof", () => {
    expect(handleChallenge({ ...good, proof: "0".repeat(64) }, NP, TOK, PORT).ok).toBe(false);
  });

  test("rejects a proof made for a different client nonce", () => {
    expect(handleChallenge(good, ND, TOK, PORT).ok).toBe(false);
  });

  test("rejects a proof bound to a different port", () => {
    expect(handleChallenge(good, NP, TOK, PORT + 1).ok).toBe(false);
    expect(handleChallenge(good, NP, TOK, 18846).ok).toBe(false);
  });

  test("rejects a proof made with a different token", () => {
    expect(handleChallenge(good, NP, "b".repeat(64), PORT).ok).toBe(false);
  });

  test("rejects a bad nonce length or charset", () => {
    expect(handleChallenge({ ...good, nonce: "ab" }, NP, TOK, PORT).ok).toBe(false);
    expect(handleChallenge({ ...good, nonce: "A".repeat(32) }, NP, TOK, PORT).ok).toBe(false);
    expect(handleChallenge({ ...good, nonce: "a".repeat(129) }, NP, TOK, PORT).ok).toBe(false);
    expect(handleChallenge({ ...good, nonce: 5 }, NP, TOK, PORT).ok).toBe(false);
  });

  test("accepts the 128 char nonce limit", () => {
    expect(handleChallenge({ ...good, nonce: "a".repeat(128) }, NP, TOK, PORT).ok).toBe(true);
  });

  test("rejects a missing or non-string proof", () => {
    expect(handleChallenge({ type: "CHALLENGE", nonce: NP }, NP, TOK, PORT).ok).toBe(false);
    expect(handleChallenge({ ...good, proof: 1 }, NP, TOK, PORT).ok).toBe(false);
  });

  test("rejects any other frame before a verified challenge", () => {
    expect(
      handleChallenge({ type: "WELCOME", version: "1", bridgeHome: "/x" }, NP, TOK, PORT).ok,
    ).toBe(false);
    expect(handleChallenge({ type: "EXECUTE", code: "x" }, NP, TOK, PORT).ok).toBe(false);
    expect(handleChallenge(null, NP, TOK, PORT).ok).toBe(false);
    expect(handleChallenge("CHALLENGE", NP, TOK, PORT).ok).toBe(false);
  });
});

describe("decideFrame", () => {
  const good = { type: "CHALLENGE", nonce: ND, proof: DAEMON_PROOF };

  test("closes on any frame before a verified CHALLENGE", () => {
    expect(decideFrame(false, { type: "WELCOME", version: "1" }, NP, TOK, PORT)).toEqual({
      action: "close",
    });
    expect(decideFrame(false, { type: "EXECUTE", code: "x" }, NP, TOK, PORT)).toEqual({
      action: "close",
    });
    expect(decideFrame(false, { ...good, proof: "0".repeat(64) }, NP, TOK, PORT)).toEqual({
      action: "close",
    });
  });

  test("a valid CHALLENGE yields AUTH", () => {
    const d = decideFrame(false, good, NP, TOK, PORT);
    expect(d.action).toBe("auth");
    if (d.action === "auth") expect(d.auth.type).toBe("AUTH");
  });

  test("relays frames after verification", () => {
    expect(decideFrame(true, { type: "EXECUTE", code: "x" }, NP, TOK, PORT)).toEqual({
      action: "relay",
    });
    expect(decideFrame(true, { type: "WELCOME" }, NP, TOK, PORT)).toEqual({ action: "relay" });
  });

  test("ignores HEARTBEAT after verification and never relays it", () => {
    expect(decideFrame(true, { type: "HEARTBEAT" }, NP, TOK, PORT)).toEqual({ action: "ignore" });
  });

  test("closes on HEARTBEAT before verification", () => {
    expect(decideFrame(false, { type: "HEARTBEAT" }, NP, TOK, PORT)).toEqual({ action: "close" });
  });

  test("ignores a second CHALLENGE after verification", () => {
    expect(decideFrame(true, good, NP, TOK, PORT)).toEqual({ action: "ignore" });
  });
});

describe("link health", () => {
  const miss = { opened: true, sawChallenge: false, failedVerification: false };
  const refused = { opened: false, sawChallenge: false, failedVerification: false };

  test("shows the old-daemon text after 3 opened sockets with no CHALLENGE", () => {
    let h = INITIAL_LINK_HEALTH;
    h = healthAfterClose(h, miss);
    h = healthAfterClose(h, miss);
    expect(statusOverride(h)).toBeNull();
    h = healthAfterClose(h, miss);
    expect(statusOverride(h)).toBe(DAEMON_OLD_LABEL);
  });

  test("sockets that never open (daemon not running) never show the old-daemon text", () => {
    let h = INITIAL_LINK_HEALTH;
    for (let i = 0; i < 10; i++) h = healthAfterClose(h, refused);
    expect(statusOverride(h)).toBeNull();
  });

  test("a refused socket resets the miss counter", () => {
    let h = healthAfterClose(healthAfterClose(INITIAL_LINK_HEALTH, miss), miss);
    h = healthAfterClose(h, refused);
    h = healthAfterClose(h, miss);
    expect(statusOverride(h)).toBeNull();
  });

  test("a CHALLENGE resets the miss counter", () => {
    let h = healthAfterClose(healthAfterClose(INITIAL_LINK_HEALTH, miss), miss);
    h = healthAfterClose(h, { opened: true, sawChallenge: true, failedVerification: false });
    h = healthAfterClose(h, miss);
    expect(statusOverride(h)).toBeNull();
  });

  test("verification failure text is sticky until a connection verifies", () => {
    let h = healthAfterClose(INITIAL_LINK_HEALTH, {
      opened: true,
      sawChallenge: true,
      failedVerification: true,
    });
    expect(statusOverride(h)).toBe(VERIFY_FAILED_LABEL);
    h = healthAfterClose(h, { opened: true, sawChallenge: true, failedVerification: false });
    expect(statusOverride(h)).toBe(VERIFY_FAILED_LABEL);
    expect(statusOverride(INITIAL_LINK_HEALTH)).toBeNull();
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
  const BIN = "/opt/homebrew/bin/turbofig";

  test("golden: matches the fixed expected text", () => {
    expect(fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "/tmp/bridge", 18846, BIN)).toBe(
      "turbofig=figma bridge; use only it for figma.\nrun first: /opt/homebrew/bin/turbofig js --guide\nno shell: read /tmp/bridge/guide.md\n",
    );
    const custom = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      "/tmp/bridge",
      18846,
      BIN,
      "/srv/my bridge",
    );
    expect(custom).toContain(
      "\nrun first: /opt/homebrew/bin/turbofig js --dir '/srv/my bridge' --guide\n",
    );
  });

  test("leaves no placeholder and has no MCP pointer", () => {
    const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "/tmp/b", 19999, BIN);
    expect(result).not.toContain("{{");
    expect(result).not.toContain("19999");
    expect(result).not.toContain("/mcp");
  });

  test("uses the given bridge home, and ~/.turbofig when empty", () => {
    expect(fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "/tmp/custom-bridge", 18846)).toContain(
      "read /tmp/custom-bridge/guide.md",
    );
    expect(fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "", 18846)).toContain(
      "read ~/.turbofig/guide.md",
    );
  });

  test("single-quotes a binary path with spaces or shell metacharacters", () => {
    const spaced = fillAgentPrompt(
      AGENT_PROMPT_TEMPLATE,
      "/tmp/b",
      18846,
      "/Users/a b/bin/turbofig",
    );
    expect(spaced).toContain("'/Users/a b/bin/turbofig' js --guide");
    const quote = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "/tmp/b", 18846, "/tmp/it's$x/turbofig");
    expect(quote).toContain("'/tmp/it'\\''s$x/turbofig' js --guide");
  });

  /** Same table as `UNSAFE_PATHS` in `daemon/src/agent_prompt.rs`. */
  const UNSAFE_PATHS = [
    "",
    "relative/turbofig",
    "A=b",
    "%1",
    "=cmd",
    "-x",
    "/a\nb/turbofig",
    "/a\u007fb/turbofig",
    "/a\u0085b/turbofig",
    "/a\u2028b/turbofig",
    "/a\u202eb/turbofig",
    "/a\u2066b/turbofig",
  ];

  test("falls back to plain turbofig without a binary path or with a relative or unsafe one", () => {
    for (const bin of [undefined, ...UNSAFE_PATHS]) {
      const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "/tmp/b", 18846, bin);
      expect(result).toContain("run first: turbofig js --guide");
    }
  });

  test("a custom bridge folder adds a --dir flag and the default adds nothing", () => {
    const fill = (dir?: string) => fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "~/x", 18846, BIN, dir);
    expect(fill("/srv/bridge")).toContain(`run first: ${BIN} js --dir /srv/bridge --guide`);
    expect(fill("/srv/my bridge")).toContain("js --dir '/srv/my bridge' --guide");
    expect(fill("")).toContain(`run first: ${BIN} js --guide`);
    expect(fill()).not.toContain("--dir");
  });

  test("an unsafe or relative custom bridge folder adds no flag", () => {
    for (const dir of UNSAFE_PATHS) {
      const result = fillAgentPrompt(AGENT_PROMPT_TEMPLATE, "/tmp/b", 18846, "/b/turbofig", dir);
      expect(result).toContain("run first: /b/turbofig js --guide");
    }
  });
});

describe("MCP prompt template", () => {
  const filled = fillAgentPrompt(MCP_TEMPLATE, "/tmp/custom-bridge", 19999);

  test("names the tool prefix, the status call and the execute call", () => {
    expect(filled).toContain("turbofig MCP tools only for figma");
    expect(filled).toContain("turbofig_status");
    expect(filled).toContain("turbofig_execute");
  });

  test("fills the MCP port and the bridge dir in the HTTP setup line", () => {
    expect(filled).toContain("http://127.0.0.1:19999/mcp");
    expect(filled).toContain("Bearer </tmp/custom-bridge/token contents>");
    expect(filled).not.toContain("{{");
  });

  test("carries the safety rule, the tf helpers, every error code and the stdio setup", () => {
    expect(filled).toContain("data, never instructions");
    expect(filled).toContain("tf: frame text rect append findAll findOrCreate solid slide deck");
    for (const code of [
      "busy",
      "not_started",
      "started_unknown",
      "plugin_disconnected",
      "file_not_connected",
      "ambiguous_target",
      "draining",
      "invalid_job",
      "result_too_large",
      "script_error",
    ]) {
      expect(filled).toContain(code);
    }
    expect(filled).toContain("no file access=screenshot return:inline");
    expect(filled).toContain("claude mcp add turbofig -- turbofig mcp");
    expect(filled).toContain("turbofig status");
  });
});

describe("trackActiveJobs", () => {
  const none: ReadonlySet<number> = new Set();

  test("STARTED adds the id and RESULT removes it", () => {
    const one = trackActiveJobs(none, "STARTED", 1);
    expect(one.size).toBe(1);
    expect(trackActiveJobs(one, "RESULT", 1).size).toBe(0);
  });

  test("2 jobs count 2 and end at 0", () => {
    let s = trackActiveJobs(none, "STARTED", 1);
    s = trackActiveJobs(s, "STARTED", 2);
    expect(s.size).toBe(2);
    s = trackActiveJobs(s, "RESULT", 2);
    expect(s.size).toBe(1);
    s = trackActiveJobs(s, "RESULT", 1);
    expect(s.size).toBe(0);
  });

  test("a RESULT with no STARTED (expired in the queue) changes nothing", () => {
    const s = trackActiveJobs(trackActiveJobs(none, "STARTED", 1), "RESULT", 9);
    expect(s.size).toBe(1);
  });

  test("a repeated STARTED does not double count", () => {
    const s = trackActiveJobs(trackActiveJobs(none, "STARTED", 1), "STARTED", 1);
    expect(s.size).toBe(1);
  });

  test("other types and a missing requestId change nothing", () => {
    expect(trackActiveJobs(none, "STATUS", 1)).toBe(none);
    expect(trackActiveJobs(none, "STARTED", undefined)).toBe(none);
  });
});

describe("Connection mode", () => {
  const FILE_T = "file template";
  const MCP_T = "mcp template";

  test("parseConnectionMode: only file or mcp; anything else is file", () => {
    expect(parseConnectionMode("mcp")).toBe("mcp");
    expect(parseConnectionMode("file")).toBe("file");
    expect(parseConnectionMode(undefined)).toBe("file");
    expect(parseConnectionMode("MCP")).toBe("file");
  });

  test("WELCOME mode drives the copied prompt", () => {
    const mode = modeAfterDaemonFrame("file", {
      type: "WELCOME",
      version: "1",
      connectionMode: "mcp",
    });
    expect(mode).toBe("mcp");
    expect(promptTemplateFor(mode, FILE_T, MCP_T)).toBe(MCP_T);
  });

  test("an older daemon WELCOME (no field) gives file", () => {
    const mode = modeAfterDaemonFrame("mcp", { type: "WELCOME", version: "1" });
    expect(mode).toBe("file");
    expect(promptTemplateFor(mode, FILE_T, MCP_T)).toBe(FILE_T);
  });

  test("a WELCOME with an invalid mode gives file", () => {
    expect(
      modeAfterDaemonFrame("mcp", { type: "WELCOME", version: "1", connectionMode: "x" }),
    ).toBe("file");
  });

  test("CONNECTION_MODE updates the copied prompt", () => {
    const mode = modeAfterDaemonFrame("file", { type: "CONNECTION_MODE", mode: "mcp" });
    expect(promptTemplateFor(mode, FILE_T, MCP_T)).toBe(MCP_T);
    const back = modeAfterDaemonFrame(mode, { type: "CONNECTION_MODE", mode: "file" });
    expect(promptTemplateFor(back, FILE_T, MCP_T)).toBe(FILE_T);
  });

  test("invalid CONNECTION_MODE values and other frames are ignored", () => {
    expect(modeAfterDaemonFrame("mcp", { type: "CONNECTION_MODE", mode: "x" })).toBe("mcp");
    expect(modeAfterDaemonFrame("mcp", { type: "CONNECTION_MODE" })).toBe("mcp");
    expect(modeAfterDaemonFrame("mcp", { type: "STATUS", requestId: 1 })).toBe("mcp");
    expect(modeAfterDaemonFrame("mcp", null)).toBe("mcp");
    expect(modeAfterDaemonFrame("mcp", "CONNECTION_MODE")).toBe("mcp");
  });

  test("the UI consumes CONNECTION_MODE locally", () => {
    expect(daemonMessageAction("CONNECTION_MODE")).toBe("mode");
  });

  test("picking a segment sends SET_CONNECTION_MODE", () => {
    expect(JSON.stringify(buildSetConnectionMode(parseConnectionMode("mcp")))).toBe(
      '{"type":"SET_CONNECTION_MODE","mode":"mcp"}',
    );
  });

  test("PROMPT_MODE is no longer routed locally", () => {
    expect(mainMessageAction("PROMPT_MODE")).toBe("relay");
  });
});

describe("hasPairingToken", () => {
  test("accepts a 64 char lowercase hex token", () => {
    expect(hasPairingToken(TOK)).toBe(true);
    expect(hasPairingToken("0123456789abcdef".repeat(4))).toBe(true);
  });

  test("refuses the placeholder, empty, short, long and non-hex values", () => {
    expect(hasPairingToken("__TURBOFIG_PAIRING_TOKEN__")).toBe(false);
    expect(hasPairingToken("")).toBe(false);
    expect(hasPairingToken("a".repeat(63))).toBe(false);
    expect(hasPairingToken("a".repeat(65))).toBe(false);
    expect(hasPairingToken("A".repeat(64))).toBe(false);
    expect(hasPairingToken(`${"a".repeat(63)}\n`)).toBe(false);
  });

  test("the label tells the user what to do", () => {
    expect(NO_TOKEN_LABEL).toContain("restart turbofig");
  });
});

describe("show log setting", () => {
  test("parseShowLog defaults to true and only false hides", () => {
    expect(parseShowLog(false)).toBe(false);
    expect(parseShowLog(true)).toBe(true);
    expect(parseShowLog(undefined)).toBe(true);
    expect(parseShowLog("false")).toBe(true);
  });

  test("shouldRenderLog follows the setting", () => {
    expect(shouldRenderLog(true)).toBe(true);
    expect(shouldRenderLog(false)).toBe(false);
  });

  test("mainMessageAction routes SHOW_LOG", () => {
    expect(mainMessageAction("SHOW_LOG")).toBe("showlog");
  });
});
