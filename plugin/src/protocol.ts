/**
 * Typed message shapes for the Turbofig daemon <-> plugin protocol.
 * All messages carry a `type` discriminant.
 */

/** Sent by the plugin to the daemon on connect to identify the open file. */
export interface FileInfoMessage {
  type: "FILE_INFO";
  fileKey: string;
  name: string;
}

/** Sent by the daemon to the plugin to request a status ping. requestId is a u64 JSON number. */
export interface StatusMessage {
  type: "STATUS";
  requestId: number;
}

/** Sent by the plugin to the daemon in reply to a command. requestId echoes the request. */
export interface ResultMessage {
  type: "RESULT";
  requestId: number;
  ok?: boolean;
  fileKey?: string;
  name?: string;
}

/** Union of all messages on the daemon <-> plugin WebSocket. */
export type DaemonMessage = FileInfoMessage | StatusMessage | ResultMessage;

/**
 * Returns true when `x` is a well-formed DaemonMessage.
 * Use this guard before handling messages received over the WebSocket.
 */
export function isDaemonMessage(x: unknown): x is DaemonMessage {
  if (typeof x !== "object" || x === null) return false;
  const msg = x as Record<string, unknown>;
  if (typeof msg.type !== "string") return false;
  switch (msg.type) {
    case "FILE_INFO":
      return typeof msg.fileKey === "string" && typeof msg.name === "string";
    case "STATUS":
      return typeof msg.requestId === "number";
    case "RESULT":
      return typeof msg.requestId === "number";
    default:
      return false;
  }
}

/**
 * Builds the RESULT reply for a STATUS request.
 * Echoes requestId and includes the current file identity.
 */
export function buildResult(status: StatusMessage, fileKey: string, name: string): ResultMessage {
  return { type: "RESULT", requestId: status.requestId, ok: true, fileKey, name };
}

/**
 * Returns the reconnect delay in milliseconds for the given attempt count.
 * Base: 500ms. Factor: 2 (exponential). Cap: 30000ms. Attempt 0 returns the base.
 */
export function backoffDelayMs(attempt: number): number {
  return Math.min(500 * 2 ** Math.max(0, attempt), 30000);
}
