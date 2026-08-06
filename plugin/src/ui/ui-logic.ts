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
 * "error" returns reconnecting — the close event fires next and drives the retry.
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
