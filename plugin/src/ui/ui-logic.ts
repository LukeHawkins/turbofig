/**
 * Pure UI logic for the turbofig plugin panel.
 * No DOM, no WebSocket. All functions are testable in isolation.
 */

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
 * "relay": forward it to the main thread (requests and everything else).
 */
export function daemonMessageAction(type: string): "welcome" | "relay" {
  if (type === "WELCOME") return "welcome";
  return "relay";
}

/**
 * Decides how the UI handles a message from the main thread.
 * "port": consume it locally to set the port and reconnect (PORT).
 * "fileinfo": store and reflect the file identity (FILE_INFO).
 * "relay": forward it over the socket (RESULT and others).
 */
export function mainMessageAction(type: string): "port" | "fileinfo" | "relay" {
  if (type === "PORT") return "port";
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
 * Returns a human-readable stale warning when plugin and daemon versions differ.
 * Returns an empty string when versions match or either is unknown.
 */
export function staleWarning(pluginVersion: string, daemonVersion: string): string {
  if (!isDaemonStale(pluginVersion, daemonVersion)) return "";
  return `Version mismatch: plugin ${pluginVersion}, daemon ${daemonVersion}. Reload the plugin.`;
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
 * Returns the WebSocket URL for the given port and pairing token.
 * Always connects to 127.0.0.1, which is the address the daemon binds.
 * The daemon rejects the upgrade with 401 when the `token` query parameter
 * is missing or wrong (see daemon/src/ws.rs), so every connect and
 * reconnect, including after a PORT switch, must carry it.
 */
export function wsUrlForPort(port: number, token: string): string {
  return `ws://127.0.0.1:${port}?token=${encodeURIComponent(token)}`;
}

/** The file-bridge home shown in the connect prompt when the daemon has not yet reported one. */
export const DEFAULT_BRIDGE_HOME = "~/.turbofig";

/** One connected Figma file, as reported by the daemon's `/health`. */
export interface ConnectedFile {
  fileKey: string;
  name: string;
}

/**
 * Fills the shared agent-connect-prompt template (`prompts/agent-prompt.txt`,
 * inlined at build time by `build-ui.ts` into `__AGENT_PROMPT_TEMPLATE__`)
 * for the given connected-file list, bridge directory and MCP port.
 *
 * The daemon's menu bar fills the identical template with the identical
 * branch (`daemon/src/agent_prompt.rs`'s `fill_agent_prompt`), so the 2
 * copies of the prompt can never drift apart; a golden test here and one
 * there assert the same fixed inputs give the same fixed output text.
 *
 * - 0 connected files: the example job's fileKey is a `<fileKey>`
 *   placeholder, with a trailing hint to run `status` first.
 * - Exactly 1: its fileKey is filled directly into the example job.
 * - More than 1: a `Connected files: ` line lists every name and fileKey,
 *   the example job keeps the placeholder, and the hint points at the list.
 *
 * `bridgeDir` falls back to `DEFAULT_BRIDGE_HOME` when empty, e.g. before
 * the first WELCOME (which carries the daemon's real `bridgeHome`) arrives.
 */
export function fillAgentPrompt(
  template: string,
  connectedFiles: ConnectedFile[],
  bridgeDir: string,
  mcpPort: number,
): string {
  let filesLine = "";
  let fileKey = "<fileKey>";
  let fileKeyHint = "";
  if (connectedFiles.length === 0) {
    fileKeyHint = " Run the status op first to learn the fileKey.";
  } else if (connectedFiles.length === 1) {
    fileKey = connectedFiles[0].fileKey;
  } else {
    const list = connectedFiles.map((f) => `${f.name} (${f.fileKey})`).join(", ");
    filesLine = `Connected files: ${list}.\n`;
    fileKeyHint = " Pick a fileKey from the list above.";
  }
  const home = bridgeDir || DEFAULT_BRIDGE_HOME;
  return template
    .split("{{FILES_LINE}}")
    .join(filesLine)
    .split("{{FILE_KEY}}")
    .join(fileKey)
    .split("{{FILE_KEY_HINT}}")
    .join(fileKeyHint)
    .split("{{BRIDGE_DIR}}")
    .join(home)
    .split("{{MCP_PORT}}")
    .join(String(mcpPort));
}

/**
 * Returns the panel dimensions for the given screen name.
 * main: compact status view (300 x 150). advanced: full settings view (300 x 240).
 * about: project info view (300 x 375).
 */
export function screenSize(name: "main" | "advanced" | "about"): { width: number; height: number } {
  if (name === "advanced") return { width: 300, height: 240 };
  if (name === "about") return { width: 300, height: 375 };
  return { width: 300, height: 150 };
}
