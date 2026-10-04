/**
 * Pure UI logic for the Turbofig plugin panel.
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
 * Accepts integers in the range 1–65535 only.
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
 * Returns the WebSocket URL for the given port.
 * Always connects to 127.0.0.1, which is the address the daemon binds.
 */
export function wsUrlForPort(port: number): string {
  return `ws://127.0.0.1:${port}`;
}

/**
 * Returns a self-bootstrapping connect prompt for Claude Code.
 * The prompt names the MCP HTTP port and the file key so a user can paste it
 * directly into Claude Code without any manual configuration.
 * Returns an empty string when fileKey is empty (no active file).
 */
export function formatConnectPrompt(fileKey: string, mcpPort: number): string {
  if (!fileKey) return "";
  return `turbofig file-bridge: write {"op":"execute","fileKey":"${fileKey}","code":"..."} to ~/.turbofig/inbox/<unique-id>.json (id unique per job) → read ~/.turbofig/outbox/<unique-id>.json. Ops: status|execute|get_selection|screenshot. MCP fallback (curl only, not web-fetch or HTTPS): http://127.0.0.1:${mcpPort}/mcp`;
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
