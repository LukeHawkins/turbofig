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

/** Sent by the daemon to the plugin to request a status ping. */
export interface StatusMessage {
  type: "STATUS";
  requestId: string;
}

/** Sent by the plugin to the daemon in reply to a command. */
export interface ResultMessage {
  type: "RESULT";
  requestId: string;
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
      return typeof msg.requestId === "string";
    case "RESULT":
      return typeof msg.requestId === "string";
    default:
      return false;
  }
}
