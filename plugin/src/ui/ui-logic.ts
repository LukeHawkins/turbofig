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

/** Known profile ids that map to named select options. */
export const KNOWN_PROFILES: readonly string[] = ["impeccable", "editorial", "minimal", "none"];

/**
 * Maps a profile id to the select element value and a flag for the custom input.
 * Returns { value: profileId, isCustom: false } for known profiles.
 * Returns { value: "custom", isCustom: true } for unknown profile ids.
 */
export function profileToSelectValue(profileId: string): { value: string; isCustom: boolean } {
  if (KNOWN_PROFILES.includes(profileId)) {
    return { value: profileId, isCustom: false };
  }
  return { value: "custom", isCustom: true };
}

/**
 * Returns a compact display line for the active file.
 * Shows the file name and the first 8 characters of the file key.
 * Returns "No file" when both inputs are empty.
 */
export function formatFileLine(fileKey: string, name: string): string {
  if (!name && !fileKey) return "No file";
  if (!name) return fileKey.slice(0, 8);
  if (!fileKey) return name;
  return `${name} (${fileKey.slice(0, 8)})`;
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

/** Result of a pairing computation: whether a Claude session drives this file, and a label. */
export interface PairingResult {
  paired: boolean;
  label: string;
}

/**
 * Returns the pairing state between a Claude session and this file.
 * Empty sessionId means no session drives this file: paired is false.
 * Non-empty sessionId means a session is active: paired is true.
 * Truncates the session id to 12 characters with "..." when it is longer.
 * Includes the file name in the label when fileName is non-empty.
 */
export function formatPairing(sessionId: string, fileName: string): PairingResult {
  if (!sessionId) {
    return {
      paired: false,
      label: "Not paired: no Claude session is driving this file yet.",
    };
  }
  const shortId = sessionId.length > 12 ? `${sessionId.slice(0, 12)}...` : sessionId;
  const filePart = fileName ? ` on ${fileName}` : "";
  return {
    paired: true,
    label: `Paired with session ${shortId}${filePart}.`,
  };
}
