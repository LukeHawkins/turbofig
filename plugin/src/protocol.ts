/**
 * Typed message shapes for the Turbofig daemon <-> plugin protocol.
 * All messages carry a `type` discriminant.
 */

/**
 * Sent by the plugin to the daemon on connect to identify the open file.
 * `pluginVersion` lets the daemon flag a stale plugin that needs reopening
 * in Figma (see `turbofig_status` and `/health`'s version-mismatch text).
 */
export interface FileInfoMessage {
  type: "FILE_INFO";
  fileKey: string;
  name: string;
  pluginVersion: string;
}

/**
 * Sent by the plugin UI iframe to the plugin main thread to change the daemon
 * WebSocket port. The main thread validates and persists the port, then sends
 * a PortMessage back to the UI. Never forwarded over the WebSocket.
 */
export interface SetPortMessage {
  type: "SET_PORT";
  port: number;
}

/**
 * Sent by the plugin main thread to the UI to report the active daemon port.
 * Emitted on bootstrap (from clientStorage) and after each SET_PORT is accepted.
 * The UI uses this to build the WebSocket URL and to update the port field.
 * This message never travels over the WebSocket.
 */
export interface PortMessage {
  type: "PORT";
  port: number;
}

/**
 * Sent by the plugin UI iframe to the plugin main thread to resize the panel.
 * The main thread calls figma.ui.resize with the given dimensions.
 * Never forwarded over the WebSocket.
 */
export interface ResizeMessage {
  type: "RESIZE";
  width: number;
  height: number;
}

/**
 * Sent by the plugin UI iframe to the plugin main thread once, on load.
 * The main thread replies with FILE_INFO and PORT. This handshake replaces an
 * eager bootstrap send, so the UI never misses either message, and the main
 * thread can re-announce FILE_INFO later on the same channel (e.g. after a
 * file rename). Never forwarded over the WebSocket.
 */
export interface ReadyMessage {
  type: "READY";
}

/**
 * Sent by the daemon on connect after it receives FILE_INFO.
 * The UI must store the version and must not forward this message to the main thread.
 */
export interface WelcomeMessage {
  type: "WELCOME";
  version: string;
  /** HTTP MCP port the daemon is listening on. Present in daemon v0.11+. Absent in older daemons. */
  mcpPort?: number;
}

/** Sent by the daemon to the plugin to request a status ping. requestId is a u64 JSON number. */
export interface StatusMessage {
  type: "STATUS";
  requestId: number;
  /** The MCP session that issued the call. Empty string for file-bridge requests. */
  sessionId?: string;
}

/** Sent by the daemon to the plugin to evaluate code in the Figma context. */
export interface ExecuteMessage {
  type: "EXECUTE";
  requestId: number;
  code: string;
  /**
   * Milliseconds to wait before the plugin gives up on this job and replies
   * ok:false. A synchronous infinite loop in the user code cannot be
   * interrupted (JS is single-threaded): this is a documented limit, not a bug.
   */
  timeoutMs?: number;
  /** The MCP session that issued the call. Empty string for file-bridge requests. */
  sessionId?: string;
}

/** Sent by the daemon to the plugin to request the current Figma selection. */
export interface GetSelectionMessage {
  type: "GET_SELECTION";
  requestId: number;
  /** Extra node properties to include alongside the base seven fields. */
  fields?: string[];
  /** How many child levels to traverse. 0 (default) returns the top-level nodes only. */
  depth?: number;
  /** The MCP session that issued the call. Empty string for file-bridge requests. */
  sessionId?: string;
  /**
   * Milliseconds budget for this job: queue wait plus run time combined,
   * measured from when the plugin received it. The plugin defaults to
   * 30000 when absent.
   */
  timeoutMs?: number;
}

/** Sent by the daemon to the plugin to request a PNG screenshot of a node. */
export interface ScreenshotMessage {
  type: "SCREENSHOT";
  requestId: number;
  scale?: number;
  nodeId?: string;
  /** The MCP session that issued the call. Empty string for file-bridge requests. */
  sessionId?: string;
  /**
   * Milliseconds budget for this job: queue wait plus run time combined,
   * measured from when the plugin received it. The plugin defaults to
   * 30000 when absent.
   */
  timeoutMs?: number;
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
  | WelcomeMessage
  | StatusMessage
  | ExecuteMessage
  | GetSelectionMessage
  | ScreenshotMessage
  | ResultMessage;

/**
 * Union of messages the plugin main thread can receive.
 * FILE_INFO and RESULT are outbound-only and are excluded.
 */
export type InboundMessage =
  | StatusMessage
  | ExecuteMessage
  | GetSelectionMessage
  | ScreenshotMessage
  | SetPortMessage
  | ResizeMessage
  | ReadyMessage;

/** Returns true when `x` is an array whose members are all strings. */
function isStringArray(x: unknown): x is string[] {
  return Array.isArray(x) && x.every((v) => typeof v === "string");
}

/**
 * Returns true when `x` is a well-formed inbound message for the plugin main thread.
 * Accepts STATUS, EXECUTE, GET_SELECTION, SCREENSHOT, SET_PORT, RESIZE, and READY.
 * Rejects outbound-only types: FILE_INFO and RESULT.
 */
export function isInboundMessage(x: unknown): x is InboundMessage {
  if (typeof x !== "object" || x === null) return false;
  const msg = x as Record<string, unknown>;
  if (typeof msg.type !== "string") return false;
  switch (msg.type) {
    case "STATUS":
      return typeof msg.requestId === "number";
    case "EXECUTE":
      return (
        typeof msg.requestId === "number" &&
        typeof msg.code === "string" &&
        (msg.timeoutMs === undefined || typeof msg.timeoutMs === "number")
      );
    case "GET_SELECTION":
      return (
        typeof msg.requestId === "number" &&
        (msg.fields === undefined || isStringArray(msg.fields)) &&
        (msg.depth === undefined || typeof msg.depth === "number") &&
        (msg.timeoutMs === undefined || typeof msg.timeoutMs === "number")
      );
    case "SCREENSHOT":
      return (
        typeof msg.requestId === "number" &&
        (msg.timeoutMs === undefined || typeof msg.timeoutMs === "number")
      );
    case "SET_PORT":
      return typeof msg.port === "number";
    case "RESIZE":
      return typeof msg.width === "number" && typeof msg.height === "number";
    case "READY":
      return true;
    default:
      return false;
  }
}

/**
 * Returns true when `x` is a well-formed message on the daemon <-> plugin
 * WebSocket. The UI iframe holds that socket, so this guards the UI's
 * onmessage handler before it relays a parsed payload to the main thread.
 */
export function isDaemonMessage(x: unknown): x is DaemonMessage {
  if (typeof x !== "object" || x === null) return false;
  const msg = x as Record<string, unknown>;
  if (typeof msg.type !== "string") return false;
  switch (msg.type) {
    case "FILE_INFO":
      return typeof msg.fileKey === "string" && typeof msg.name === "string";
    case "WELCOME":
      return typeof msg.version === "string";
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
 * Builds a FILE_INFO message for the given file key and document name.
 * Use this in the plugin main thread to post file identity to the UI.
 */
export function buildFileInfo(
  fileKey: string,
  name: string,
  pluginVersion: string,
): FileInfoMessage {
  return { type: "FILE_INFO", fileKey, name, pluginVersion };
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
 * Maximum child levels `serializeNode` will traverse.
 * Callers that pass a higher depth receive exactly this many levels.
 */
export const MAX_SELECTION_DEPTH = 5;

/**
 * Serialises a loosely-typed node object into a SelectionItem extended with
 * caller-requested extra fields and optional child traversal.
 *
 * Always includes the base seven fields (id first) via `toSelectionItem`.
 * For each name in `fields`, copies the property from `node` if present and
 * JSON-safe (skips functions and undefined values).
 * When `depth > 0` and the node has a `children` array, adds a `children`
 * array where each child is serialised with `depth - 1`.
 * Clamps `depth` to `MAX_SELECTION_DEPTH` so callers cannot force a huge tree.
 */
export function serializeNode(
  node: Record<string, unknown>,
  fields: string[] | undefined,
  depth: number,
): SelectionItem & Record<string, unknown> {
  const clampedDepth = Math.min(depth, MAX_SELECTION_DEPTH);
  const result: SelectionItem & Record<string, unknown> = { ...toSelectionItem(node) };

  // Copy requested extra fields. Skip children (owned by the depth mechanism),
  // functions, undefined values, and values that are not JSON-safe (e.g. Figma
  // node proxies, which are not structured-cloneable and would throw on postMessage).
  if (fields) {
    for (const field of fields) {
      // The depth mechanism owns children; never let fields bypass the depth cap.
      if (field === "children") continue;
      // A Figma node property can be a getter that throws under dynamic-page
      // (e.g. mainComponent on a non-instance). Isolate the read so one bad
      // field cannot fail the whole GET_SELECTION response.
      let val: unknown;
      try {
        val = node[field];
      } catch {
        continue;
      }
      if (val === undefined || typeof val === "function") continue;
      if (
        val === null ||
        typeof val === "boolean" ||
        typeof val === "number" ||
        typeof val === "string"
      ) {
        // Primitives are always JSON-safe; copy directly.
        result[field] = val;
      } else {
        // Object or array: verify JSON-safety before copying.
        // Non-cloneable proxies and circular references are caught and dropped.
        try {
          result[field] = JSON.parse(JSON.stringify(val));
        } catch {
          // Not JSON-safe: skip silently.
        }
      }
    }
  }

  // Recurse into children when depth allows.
  if (clampedDepth > 0 && Array.isArray(node.children)) {
    result.children = (node.children as Record<string, unknown>[]).map((child) =>
      serializeNode(child, fields, clampedDepth - 1),
    );
  }

  return result;
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

/** Base64 alphabet, RFC 4648 standard (with padding). */
const BASE64_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/**
 * Encodes bytes as base64.
 * Hand-rolled (no btoa/Buffer): the Figma plugin main thread sandbox does not
 * guarantee either, and this must also run under bun test.
 */
export function encodeBase64(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] ?? 0;
    const b1 = bytes[i + 1];
    const b2 = bytes[i + 2];
    const triple = (b0 << 16) | ((b1 ?? 0) << 8) | (b2 ?? 0);
    out += BASE64_CHARS[(triple >> 18) & 0x3f];
    out += BASE64_CHARS[(triple >> 12) & 0x3f];
    out += b1 === undefined ? "=" : BASE64_CHARS[(triple >> 6) & 0x3f];
    out += b2 === undefined ? "=" : BASE64_CHARS[triple & 0x3f];
  }
  return out;
}

/**
 * Returns the UTF-8 byte length of a string.
 * Hand-rolled (no TextEncoder): same sandbox-portability reason as encodeBase64.
 */
export function utf8ByteLength(str: string): number {
  let bytes = 0;
  for (let i = 0; i < str.length; i++) {
    const code = str.charCodeAt(i);
    if (code <= 0x7f) bytes += 1;
    else if (code <= 0x7ff) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff) {
      bytes += 4;
      i++; // consume the low surrogate half of the pair
    } else bytes += 3;
  }
  return bytes;
}

/** JSON.stringify replacer: base64-encodes a Uint8Array or ArrayBuffer in place. */
function replaceBinary(_key: string, val: unknown): unknown {
  if (val instanceof Uint8Array) return encodeBase64(val);
  if (val instanceof ArrayBuffer) return encodeBase64(new Uint8Array(val));
  return val;
}

/**
 * Returns a JSON-serializable form of value.
 * `undefined` becomes `null` (JSON has no `undefined`; the old behaviour sent
 * the literal string "undefined"). A Uint8Array or ArrayBuffer (e.g. from
 * `tf.export`) is base64-encoded, matching the `png` field convention used
 * elsewhere in this protocol. On failure (e.g. a circular reference) it falls
 * back to String(value).
 */
export function safeResult(value: unknown): unknown {
  if (value === undefined) return null;
  let json: string | undefined;
  try {
    json = JSON.stringify(value, replaceBinary);
  } catch {
    return String(value);
  }
  if (json === undefined) return String(value);
  return JSON.parse(json);
}

/** Maximum size, in bytes, of a serialized RESULT message. See capResultMessage. */
export const MAX_RESULT_BYTES = 16 * 1024 * 1024;

/**
 * Returns `msg` unchanged when it serializes at or under MAX_RESULT_BYTES.
 * Otherwise returns an error RESULT with the same requestId, so an oversized
 * EXECUTE/GET_SELECTION/SCREENSHOT reply never reaches the WebSocket.
 * This is the single enforcement point for the protocol's 16 MiB result cap.
 */
export function capResultMessage(msg: ResultMessage): ResultMessage {
  let json: string;
  try {
    json = JSON.stringify(msg);
  } catch {
    return msg; // a handler never builds a circular ResultMessage; defensive only
  }
  const bytes = utf8ByteLength(json);
  if (bytes <= MAX_RESULT_BYTES) return msg;
  const mib = (bytes / (1024 * 1024)).toFixed(1);
  return buildExecuteError(
    msg.requestId,
    `result too large (${mib} MiB > 16 MiB); return less data or use file mode`,
  );
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

/**
 * Number of lines the preamble occupies in a wrapUserCode result, before the
 * user's own code starts. Derived from DEPRECATION_PREAMBLE so the two values
 * can never drift apart. Subtract this from a line number reported against
 * wrapped source to recover the line in the user's original code.
 */
export const PREAMBLE_LINE_OFFSET = DEPRECATION_PREAMBLE.split("\n").length;
