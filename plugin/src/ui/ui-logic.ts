/**
 * Pure UI logic for the turbofig plugin panel.
 * No DOM, no WebSocket. All functions are testable in isolation.
 */

import { asConnectionMode, type ConnectionMode } from "../protocol";
import { constantTimeEqual, hmacSha256Hex, toHex } from "./hmac";

/** Connection state values. */
export type ConnState = "connecting" | "connected" | "reconnecting" | "offline";

/** WebSocket lifecycle events the state model recognises. */
export type WsEvent = "open" | "close" | "error" | "attempt";

/** Result of a state computation: the state and a display label. */
export interface ConnStateResult {
  state: ConnState;
  label: string;
}

/**
 * Computes the connection state and display label from a WS event and attempt count.
 * Call this on each WS lifecycle event. Pass the current attempt counter.
 * Attempt 0 means the initial connect. Offline threshold: 5 or more failed attempts.
 * "error" returns reconnecting: the close event fires next and drives the retry.
 */
export function connStateFromEvent(event: WsEvent, attempt: number): ConnStateResult {
  if (event === "open") {
    return { state: "connected", label: "Connected" };
  }
  if (event === "attempt" || event === "close") {
    if (attempt === 0) return { state: "connecting", label: "Connecting..." };
    if (attempt >= 5) return { state: "offline", label: "Offline" };
    return { state: "reconnecting", label: "Reconnecting..." };
  }
  // "error": close fires after this; keep the label consistent.
  return { state: "reconnecting", label: "Reconnecting..." };
}

/**
 * Returns a display string for the active MCP session id.
 * Returns "No active session" when sessionId is empty.
 * Truncates ids longer than 12 characters with "..." appended.
 */
export function formatSession(sessionId: string): string {
  if (!sessionId) return "No active session";
  if (sessionId.length > 12) return `${sessionId.slice(0, 12)}...`;
  return sessionId;
}

/** Daemon request types that the UI relays and records in the activity log. */
export const REQUEST_TYPES = ["STATUS", "EXECUTE", "GET_SELECTION", "SCREENSHOT"] as const;

/** Returns true when the type is a daemon request shown in the activity log. */
export function isRequestType(type: string): boolean {
  return (REQUEST_TYPES as readonly string[]).includes(type);
}

/**
 * Decides how the UI handles a message received from the daemon over the socket.
 * "welcome": consume it locally to read the daemon version (WELCOME).
 * "mode": consume it locally to update the Connection mode (CONNECTION_MODE).
 * "drop": never forward it (CLOSE_FOR_UPDATE is UI-only).
 * "relay": forward it to the main thread (requests and everything else).
 */
export function daemonMessageAction(type: string): "welcome" | "mode" | "drop" | "relay" {
  /* Only the UI's own Reload button may close the plugin: never the daemon. */
  if (type === "CLOSE_FOR_UPDATE") return "drop";
  /* The update check is local to the plugin: the daemon never drives it. */
  if (isUpdateUiType(type) || type === "UPDATE_STATE") return "drop";
  if (type === "WELCOME") return "welcome";
  if (type === "CONNECTION_MODE") return "mode";
  return "relay";
}

/** Update check types only the UI itself sends to the main thread. */
function isUpdateUiType(type: unknown): boolean {
  return type === "SET_UPDATE_CHECK" || type === "SET_UPDATE_LATEST" || type === "DISMISS_UPDATE";
}

/**
 * Control types only the UI itself may send to the daemon, or only the daemon
 * sends. Code run by an EXECUTE job can post any message from the main thread,
 * so the relay drops these: only the Connection segment click handler sends
 * SET_CONNECTION_MODE, and only the UI handshake sends AUTH.
 */
export function isUiOnlyControlType(type: unknown): boolean {
  return (
    type === "SET_CONNECTION_MODE" ||
    type === "AUTH" ||
    type === "CONNECTION_MODE" ||
    type === "WELCOME" ||
    type === "CHALLENGE" ||
    type === "CLOSE_FOR_UPDATE" ||
    isUpdateUiType(type)
  );
}

/**
 * Decides how the UI handles a message from the main thread.
 * "port": consume it locally to set the port and reconnect (PORT).
 * "updatestate": consume it locally: the stored update check values (UPDATE_STATE).
 * "fileinfo": store and reflect the file identity (FILE_INFO).
 * "drop": never relay it (a UI-only control type, see isUiOnlyControlType).
 * "relay": forward it over the socket (RESULT and others).
 */
export function mainMessageAction(
  type: string,
): "port" | "showlog" | "updatestate" | "fileinfo" | "drop" | "relay" {
  if (type === "UPDATE_STATE") return "updatestate";
  if (isUiOnlyControlType(type)) return "drop";
  if (type === "PORT") return "port";
  if (type === "SHOW_LOG") return "showlog";
  if (type === "FILE_INFO") return "fileinfo";
  return "relay";
}

/**
 * Decides what the UI should do with an incoming PORT message.
 * "connect": the first PORT ever received (the reply to the on-load READY
 * handshake) - open the first socket.
 * "reconnect": a later PORT naming a different port (the user changed it) -
 * close the old socket and open a new one.
 * "none": a later PORT naming the same port - nothing to do.
 */
export function portMessageAction(
  hasConnectedOnce: boolean,
  currentPort: number,
  newPort: number,
): "connect" | "reconnect" | "none" {
  if (!hasConnectedOnce) return "connect";
  if (newPort !== currentPort) return "reconnect";
  return "none";
}

/**
 * Returns a new array with entry appended and trimmed to the last cap items.
 * The input array is never mutated.
 * cap must be >= 1; a cap of 0 always returns an empty array.
 */
export function appendLog<T>(log: T[], entry: T, cap: number): T[] {
  const next = [...log, entry];
  if (next.length > cap) return next.slice(next.length - cap);
  return next;
}

/**
 * Returns a compact one-line label for a daemon request.
 * Time is expressed in UTC so the output is deterministic for a fixed ts.
 * Format: HH:MM:SS TYPE (e.g. "14:05:02 EXECUTE").
 */
export function formatLogEntry(type: string, ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getUTCHours()).padStart(2, "0");
  const mm = String(d.getUTCMinutes()).padStart(2, "0");
  const ss = String(d.getUTCSeconds()).padStart(2, "0");
  return `${hh}:${mm}:${ss} ${type}`;
}

/**
 * Returns true only when both version strings are non-empty and differ.
 * Either string being empty means the daemon version is unknown: not stale.
 */
export function isDaemonStale(pluginVersion: string, daemonVersion: string): boolean {
  if (!pluginVersion || !daemonVersion) return false;
  return pluginVersion !== daemonVersion;
}

/**
 * Returns true when the daemon version is older than the plugin version.
 * Compares dotted numeric parts. An unparseable version counts as not older,
 * so the banner then offers Reload instead of a wrong "update turbofig".
 */
export function isDaemonOlder(pluginVersion: string, daemonVersion: string): boolean {
  const parts = (v: string): number[] | null => {
    const m = /^v?(\d+(?:\.\d+)*)/.exec(v.trim());
    return m ? (m[1] as string).split(".").map(Number) : null;
  };
  const p = parts(pluginVersion);
  const d = parts(daemonVersion);
  if (!p || !d) return false;
  for (let i = 0; i < Math.max(p.length, d.length); i++) {
    const a = p[i] ?? 0;
    const b = d[i] ?? 0;
    if (a !== b) return b < a;
  }
  return false;
}

/** The version banner: its text, and whether it offers the Reload button. */
export interface StaleBanner {
  text: string;
  /** True when the daemon is newer: reopening the plugin fixes it. */
  canReload: boolean;
}

/**
 * Returns the banner for a version difference, or null when versions match or
 * either is unknown. A newer daemon gets a Reload button. A newer plugin does
 * not: the user must update turbofig.
 */
export function staleBanner(pluginVersion: string, daemonVersion: string): StaleBanner | null {
  if (!isDaemonStale(pluginVersion, daemonVersion)) return null;
  if (isDaemonOlder(pluginVersion, daemonVersion)) {
    return {
      text: `turbofig ${daemonVersion} is older than this plugin. Update turbofig.`,
      canReload: false,
    };
  }
  return { text: `turbofig was updated to ${daemonVersion}.`, canReload: true };
}

/**
 * Returns the banner text for a version difference, or an empty string when
 * versions match or either is unknown.
 */
export function staleWarning(pluginVersion: string, daemonVersion: string): string {
  return staleBanner(pluginVersion, daemonVersion)?.text ?? "";
}

/** True when the Reload button must be disabled: a job is running. */
export function reloadDisabled(activeJobCount: number): boolean {
  return activeJobCount > 0;
}

/**
 * Parses a port input string and returns a valid integer port, or null.
 * Accepts integers in the range 1-65535 only.
 * Rejects empty input, non-numeric strings, and fractional numbers (e.g. "8080.5").
 */
export function parsePort(input: string): number | null {
  const trimmed = input.trim();
  if (!trimmed) return null;
  const n = Number(trimmed);
  if (!Number.isInteger(n)) return null;
  if (n < 1 || n > 65535) return null;
  return n;
}

/**
 * Returns the WebSocket URL for the given port and client nonce.
 * Always connects to 127.0.0.1, which is the address the daemon binds.
 * The pairing token never goes in the URL: the daemon proves it knows the
 * token in a CHALLENGE frame (see `handleChallenge`).
 */
export function buildSocketUrl(port: number, nonce: string): string {
  return `ws://127.0.0.1:${port}/?nonce=${nonce}`;
}

/** Returns a fresh client nonce: 16 random bytes as 32 lowercase hex chars. */
export function generateNonce(random: (bytes: Uint8Array) => Uint8Array): string {
  return toHex(random(new Uint8Array(16)));
}

/** Status text shown when the plugin build carries no real pairing token. */
export const NO_TOKEN_LABEL =
  "plugin has no pairing token: restart turbofig, then reopen the plugin";

/**
 * True when `token` looks like a real pairing token (64 lowercase hex chars).
 * The public placeholder fails this, so a build that still carries it never
 * opens a socket: anyone can compute an HMAC under a known placeholder.
 */
export function hasPairingToken(token: string): boolean {
  return /^[0-9a-f]{64}$/.test(token);
}

/** Status text shown when the daemon fails the challenge handshake. */
export const VERIFY_FAILED_LABEL =
  "turbofig was reset, or another turbofig uses its port. Close and reopen this plugin.";

export type ChallengeResult = { ok: true; auth: { type: "AUTH"; proof: string } } | { ok: false };

/**
 * Verifies the first daemon frame. The frame must be a CHALLENGE whose proof
 * equals HMAC-SHA256(token, "turbofig-ws-daemon:" + nonceP + ":" + port),
 * where `port` is the port the plugin connected to, and whose nonce
 * is 32-128 hex chars. On success returns the AUTH frame to send back.
 * Any other frame, or a bad proof, returns `{ ok: false }`.
 */
export function handleChallenge(
  frame: unknown,
  nonceP: string,
  token: string,
  port: number,
): ChallengeResult {
  if (typeof frame !== "object" || frame === null) return { ok: false };
  const f = frame as Record<string, unknown>;
  if (f.type !== "CHALLENGE") return { ok: false };
  if (typeof f.nonce !== "string" || !/^[0-9a-f]{32,128}$/.test(f.nonce)) return { ok: false };
  if (typeof f.proof !== "string") return { ok: false };
  const expected = hmacSha256Hex(token, `turbofig-ws-daemon:${nonceP}:${port}`);
  if (!constantTimeEqual(f.proof, expected)) return { ok: false };
  return {
    ok: true,
    auth: { type: "AUTH", proof: hmacSha256Hex(token, `turbofig-ws-plugin:${f.nonce}`) },
  };
}

/** What the UI does with one daemon frame. */
export type FrameDecision =
  | { action: "close" }
  | { action: "auth"; auth: { type: "AUTH"; proof: string } }
  | { action: "relay" }
  | { action: "ignore" };

/**
 * Decides what to do with a parsed daemon frame.
 * Before verification only a valid CHALLENGE is accepted ("auth"); anything
 * else means "close". After verification a second CHALLENGE and a HEARTBEAT are
 * "ignore" and every other frame is "relay".
 */
export function decideFrame(
  verified: boolean,
  frame: unknown,
  nonceP: string,
  token: string,
  port: number,
): FrameDecision {
  if (!verified) {
    const r = handleChallenge(frame, nonceP, token, port);
    return r.ok ? { action: "auth", auth: r.auth } : { action: "close" };
  }
  if (
    typeof frame === "object" &&
    frame !== null &&
    (frame as { type?: unknown }).type === "CHALLENGE"
  ) {
    return { action: "ignore" };
  }
  // HEARTBEAT only proves the link is alive. It is never a job.
  if (
    typeof frame === "object" &&
    frame !== null &&
    (frame as { type?: unknown }).type === "HEARTBEAT"
  ) {
    return { action: "ignore" };
  }
  return { action: "relay" };
}

/** Status text for a daemon that accepts the socket but never sends a CHALLENGE (an older daemon). */
export const DAEMON_OLD_LABEL =
  "Another app may be using turbofig's port, or turbofig is older than this plugin.";

/** Status text when nothing listens on the port. Names the menu bar app and Terminal. */
export const NOT_RUNNING_LABEL =
  "turbofig is not running. Open the turbofig app, or run turbofig in Terminal.";

/** Closes in a row with no CHALLENGE before the old-daemon text shows. */
export const NO_CHALLENGE_LIMIT = 3;

/** Handshake health across reconnects. */
export interface LinkHealth {
  /** Consecutive opened sockets that closed with no CHALLENGE frame. */
  misses: number;
  /** True after a failed verification, until a connection verifies. */
  failed: boolean;
}

export const INITIAL_LINK_HEALTH: LinkHealth = { misses: 0, failed: false };

/**
 * Returns the new health after a socket closes.
 *
 * Only a socket that OPENED and then closed with no CHALLENGE counts as a
 * miss: that is an older daemon. A socket that never opened means nothing
 * listens (the daemon is not running); it resets the count, so the normal
 * "Daemon not running" label shows instead of the old-daemon text.
 */
export function healthAfterClose(
  h: LinkHealth,
  outcome: { opened: boolean; sawChallenge: boolean; failedVerification: boolean },
): LinkHealth {
  let misses = 0;
  if (!outcome.sawChallenge && outcome.opened) misses = h.misses + 1;
  /* A socket that never opened means nothing listens: an earlier verify
     failure is stale, so the not-running label must show. */
  const failed = outcome.opened ? h.failed || outcome.failedVerification : false;
  return { misses, failed };
}

/** Returns the sticky status text to show instead of the normal label, or null. */
export function statusOverride(h: LinkHealth): string | null {
  if (h.failed) return VERIFY_FAILED_LABEL;
  if (h.misses >= NO_CHALLENGE_LIMIT) return DAEMON_OLD_LABEL;
  return null;
}

/** The file-bridge home shown in the connect prompt when the daemon has not yet reported one. */
export const DEFAULT_BRIDGE_HOME = "~/.turbofig";

/**
 * Renders an absolute path for a pasted shell command. A path of only safe
 * characters stays bare. Any other path is single-quoted. A path that is not
 * absolute, or holds an unsafe character (C0, DEL, C1, line and paragraph
 * separators, bidi controls), gives null. Mirrors `shell_path` in
 * `daemon/src/agent_prompt.rs`.
 */
export function shellPath(path: string): string | null {
  // biome-ignore lint/suspicious/noControlCharactersInRegex: rejecting control characters is the point.
  const unsafeChar = /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u202a-\u202e\u2066-\u2069]/;
  if (!path.startsWith("/") || unsafeChar.test(path)) return null;
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(path)) return path;
  return `'${path.split("'").join("'\\''")}'`;
}

/** The binary for the prompt: `shellPath`, else plain `turbofig`. */
export function shellBin(bin: string): string {
  return shellPath(bin) ?? "turbofig";
}

/**
 * Fills the shared agent-connect-prompt template (`prompts/agent-prompt.txt`,
 * inlined at build time by `build-ui.ts` into `__AGENT_PROMPT_TEMPLATE__`)
 * for the given bridge directory and MCP port.
 *
 * The daemon's menu bar fills the identical template with the identical
 * rules (`daemon/src/agent_prompt.rs`'s `fill_agent_prompt`), so the 2
 * copies of the prompt can never drift apart; a golden test here and one
 * there assert the same fixed inputs give the same fixed output text.
 *
 * `turbofigBin` is the daemon's stable binary path from WELCOME; empty (older
 * daemon) renders plain `turbofig`.
 * `customBridgeDir` is the absolute bridge folder, sent only when it is not
 * the default; it adds ` --dir <folder>` to the `js --guide` line.
 * `bridgeDir` falls back to `DEFAULT_BRIDGE_HOME` when empty, e.g. before
 * the first WELCOME (which carries the daemon's real `bridgeHome`) arrives.
 */
export function fillAgentPrompt(
  template: string,
  bridgeDir: string,
  mcpPort: number,
  turbofigBin = "",
  customBridgeDir = "",
): string {
  const home = bridgeDir || DEFAULT_BRIDGE_HOME;
  const customPath = shellPath(customBridgeDir);
  const bridgeFlag = customPath === null ? "" : ` --dir ${customPath}`;
  return template
    .split("{{BRIDGE_DIR}}")
    .join(home)
    .split("{{TURBOFIG_BIN}}")
    .join(shellBin(turbofigBin))
    .split("{{BRIDGE_FLAG}}")
    .join(bridgeFlag)
    .split("{{MCP_PORT}}")
    .join(String(mcpPort));
}

/**
 * Adds the file the prompt was copied from as a last line: the default
 * target, so an agent with several connected files knows which one the user
 * means. It is not a lock: the guide lists every connected file, and the
 * agent may work on (and compare) any of them when the user asks. Only a key the daemon
 * accepts (`[A-Za-z0-9_-]`, the line is pasted near a shell command) is
 * added; any other key leaves the prompt unchanged. A trailing newline is
 * kept as it was.
 */
export function withFileHint(prompt: string, fileKey: string, mode: ConnectionMode): string {
  if (!/^[A-Za-z0-9_-]{1,256}$/.test(fileKey)) return prompt;
  const use = mode === "mcp" ? "fileKey" : `--file ${fileKey}`;
  const body = prompt.replace(/\n+$/, "");
  const end = prompt.endsWith("\n") ? "\n" : "";
  return `${body}\ndefault file: ${fileKey} (${use} if files>1); other connected files ok if user asks${end}`;
}

/** Height of the "Connect to change the Connection setting." hint (text + gap). */
export const ADV_HINT_HEIGHT = 20;
/** Advanced screen height with the activity log hidden and the hint shown (rows only). */
export const ADV_HEIGHT = 451;
/** Advanced screen height with the activity log shown and the hint shown. */
export const ADV_HEIGHT_LOG = 565;
/** Height the update banner adds to the main screen (banner + gap). */
export const UPDATE_BANNER_HEIGHT = 40;
/** Height of the update screen. */
export const UPDATE_HEIGHT = 226;

/**
 * Returns the panel dimensions for the given screen name.
 * main: compact status view (300 x 150), plus UPDATE_BANNER_HEIGHT while the update banner shows. advanced: full settings view (ADV_HEIGHT_LOG, or ADV_HEIGHT with the activity log hidden), minus ADV_HINT_HEIGHT when the hint is hidden (daemon ready).
 * about: project info view (300 x 375). update: how to update turbofig (UPDATE_HEIGHT).
 */
export function screenSize(
  name: "main" | "advanced" | "about" | "update",
  showLog = true,
  hintShown = true,
  updateBannerShown = false,
): { width: number; height: number } {
  if (name === "advanced") {
    const base = showLog ? ADV_HEIGHT_LOG : ADV_HEIGHT;
    return { width: 300, height: hintShown ? base : base - ADV_HINT_HEIGHT };
  }
  if (name === "about") return { width: 300, height: 375 };
  if (name === "update") return { width: 300, height: UPDATE_HEIGHT };
  return { width: 300, height: 150 + (updateBannerShown ? UPDATE_BANNER_HEIGHT : 0) };
}

/** Returns the daemon's Connection mode when valid, else the default "file". */
export function parseConnectionMode(x: unknown): ConnectionMode {
  return asConnectionMode(x) ?? "file";
}

/**
 * Returns the Connection mode after a daemon frame. WELCOME sets it (absent or
 * invalid: "file", for an older daemon). CONNECTION_MODE sets it when the mode
 * is valid and is ignored otherwise. Any other frame leaves it unchanged.
 */
export function modeAfterDaemonFrame(current: ConnectionMode, frame: unknown): ConnectionMode {
  if (typeof frame !== "object" || frame === null) return current;
  const f = frame as Record<string, unknown>;
  if (f.type === "WELCOME") return parseConnectionMode(f.connectionMode);
  if (f.type === "CONNECTION_MODE") return asConnectionMode(f.mode) ?? current;
  return current;
}

/**
 * True when a WELCOME frame carries a valid connectionMode. Only then is the
 * daemon ready to take a mode change (an older daemon sends none).
 */
export function welcomeHasValidMode(frame: unknown): boolean {
  if (typeof frame !== "object" || frame === null) return false;
  const f = frame as Record<string, unknown>;
  return f.type === "WELCOME" && asConnectionMode(f.connectionMode) !== null;
}

/** Picks the connect prompt template that follows the Connection mode. */
export function promptTemplateFor(
  mode: ConnectionMode,
  fileTemplate: string,
  mcpTemplate: string,
): string {
  return mode === "mcp" ? mcpTemplate : fileTemplate;
}

/** Returns the stored log setting when it is a boolean, else the default true. */
export function parseShowLog(x: unknown): boolean {
  return x !== false;
}

/** True when a new entry must be rendered to the log element. */
export function shouldRenderLog(showLog: boolean): boolean {
  return showLog;
}

/**
 * Updates the set of running job ids from a relayed main-thread message.
 * STARTED adds its requestId, RESULT removes it. Other messages change
 * nothing. Tracking ids (not a bare counter) keeps the count right when a
 * job expires in the queue: it sends RESULT but never STARTED.
 * Returns the same set when nothing changed.
 */
export function trackActiveJobs(
  active: ReadonlySet<number>,
  type: unknown,
  requestId: unknown,
): ReadonlySet<number> {
  if (typeof requestId !== "number") return active;
  if (type === "STARTED" && !active.has(requestId)) return new Set(active).add(requestId);
  if (type === "RESULT" && active.has(requestId)) {
    const next = new Set(active);
    next.delete(requestId);
    return next;
  }
  return active;
}

/** Builds the message that asks the main thread to open an external URL. */
export function buildOpenUrlMessage(url: string): { type: "OPEN_URL"; url: string } {
  return { type: "OPEN_URL", url };
}
