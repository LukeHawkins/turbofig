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

/** Sent by the daemon to the plugin to evaluate code in the Figma context. */
export interface ExecuteMessage {
  type: "EXECUTE";
  requestId: number;
  code: string;
}

/** Sent by the daemon to the plugin to request the current Figma selection. */
export interface GetSelectionMessage {
  type: "GET_SELECTION";
  requestId: number;
}

/** Sent by the daemon to the plugin to request a PNG screenshot of a node. */
export interface ScreenshotMessage {
  type: "SCREENSHOT";
  requestId: number;
  scale?: number;
  nodeId?: string;
}

/** A single selected Figma node, serialised for the selection response. */
export interface SelectionItem {
  id: string;
  name: string;
  type: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Sent by the plugin to the daemon in reply to a command. requestId echoes the request. */
export interface ResultMessage {
  type: "RESULT";
  requestId: number;
  ok?: boolean;
  fileKey?: string;
  name?: string;
  result?: unknown;
  error?: string;
  selection?: SelectionItem[];
  png?: string;
  w?: number;
  h?: number;
}

/** Union of all messages on the daemon <-> plugin WebSocket. */
export type DaemonMessage =
  | FileInfoMessage
  | StatusMessage
  | ExecuteMessage
  | GetSelectionMessage
  | ScreenshotMessage
  | ResultMessage;

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
    case "EXECUTE":
      return typeof msg.requestId === "number" && typeof msg.code === "string";
    case "GET_SELECTION":
      return typeof msg.requestId === "number";
    case "SCREENSHOT":
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

/**
 * Builds a success RESULT reply for an EXECUTE request.
 * Echoes requestId and carries the return value of the executed code.
 */
export function buildExecuteSuccess(requestId: number, result: unknown): ResultMessage {
  return { type: "RESULT", requestId, ok: true, result };
}

/**
 * Builds a failure RESULT reply for an EXECUTE request.
 * Echoes requestId and carries the error message string.
 */
export function buildExecuteError(requestId: number, message: string): ResultMessage {
  return { type: "RESULT", requestId, ok: false, error: message };
}

/**
 * Maps a loosely-typed node object to a SelectionItem.
 * Reads id, name, type (strings) and x, y, width->w, height->h (numbers).
 * Each missing or wrong-type field falls back to "" (strings) or 0 (numbers).
 * Accepts Record<string, unknown> so it is unit-testable with plain objects.
 */
export function toSelectionItem(node: Record<string, unknown>): SelectionItem {
  const str = (key: string): string => (typeof node[key] === "string" ? (node[key] as string) : "");
  const num = (key: string): number => (typeof node[key] === "number" ? (node[key] as number) : 0);
  return {
    id: str("id"),
    name: str("name"),
    type: str("type"),
    x: num("x"),
    y: num("y"),
    w: num("width"),
    h: num("height"),
  };
}

/**
 * Builds a success RESULT reply for a GET_SELECTION request.
 * Echoes requestId and carries the selection array.
 */
export function buildSelection(requestId: number, selection: SelectionItem[]): ResultMessage {
  return { type: "RESULT", requestId, ok: true, selection };
}

/**
 * Builds a success RESULT reply for a SCREENSHOT request.
 * Echoes requestId and carries the PNG as a base64 string with dimensions.
 */
export function buildScreenshot(
  requestId: number,
  png: string,
  w: number,
  h: number,
): ResultMessage {
  return { type: "RESULT", requestId, ok: true, png, w, h };
}

/**
 * Returns a JSON-serializable form of value.
 * On failure (e.g. circular reference) it falls back to String(value).
 */
export function safeResult(value: unknown): unknown {
  try {
    return JSON.parse(JSON.stringify(value));
  } catch {
    return String(value);
  }
}

/**
 * Sync-to-async deprecation preamble for the eval context.
 *
 * The manifest sets `documentAccess: dynamic-page`. Under that mode Figma
 * rejects the old synchronous API calls. This preamble runs first in every
 * eval. It sets strict mode and lists the async replacements, so generated
 * code uses the async APIs from the first eval.
 *
 * Keep this table current. Phase 12 owns the monthly upkeep note.
 */
export const DEPRECATION_PREAMBLE = `"use strict";
/* Turbofig eval runs under documentAccess: dynamic-page. Use the async APIs.
   Do not use the deprecated synchronous calls on the left.
     figma.getNodeById(id)              -> await figma.getNodeByIdAsync(id)
     figma.getStyleById(id)             -> await figma.getStyleByIdAsync(id)
     figma.getLocalPaintStyles()        -> await figma.getLocalPaintStylesAsync()
     figma.getLocalTextStyles()         -> await figma.getLocalTextStylesAsync()
     figma.getLocalEffectStyles()       -> await figma.getLocalEffectStylesAsync()
     figma.getLocalGridStyles()         -> await figma.getLocalGridStylesAsync()
     figma.importComponentByKey(k)      -> await figma.importComponentByKeyAsync(k)
     figma.importComponentSetByKey(k)   -> await figma.importComponentSetByKeyAsync(k)
     figma.importStyleByKey(k)          -> await figma.importStyleByKeyAsync(k)
     instance.getMainComponent()        -> await instance.getMainComponentAsync()
     figma.variables.getVariableById(i) -> await figma.variables.getVariableByIdAsync(i)
     figma.variables.getLocalVariables()-> await figma.variables.getLocalVariablesAsync()
     figma.currentPage = page           -> await figma.setCurrentPageAsync(page)
   Load pages before you read them: await figma.loadAllPagesAsync(). */`;

/**
 * Wraps user code with the deprecation preamble for the eval context.
 * The preamble runs first, then the user code, in one async function body.
 */
export function wrapUserCode(code: string): string {
  return `${DEPRECATION_PREAMBLE}\n${code}`;
}
