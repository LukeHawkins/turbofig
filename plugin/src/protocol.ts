/**
 * Typed message shapes for the Turbofig daemon <-> plugin protocol.
 * All messages carry a `type` discriminant.
 */

/** Sent by the plugin to the daemon on connect to identify the open file. */
export interface FileInfoMessage {
  type: "FILE_INFO";
  fileKey: string;
  name: string;
  /** Active taste profile for this file. Empty string when no profile is set. */
  profileId: string;
}

/**
 * Sent by the plugin UI iframe to the plugin main thread to set the active
 * taste profile for the file. This message is local to the plugin. The UI must
 * never forward it over the WebSocket to the daemon. It shares the main thread
 * inbound channel with daemon messages, so `isDaemonMessage` guards it too.
 */
export interface SetProfileMessage {
  type: "SET_PROFILE";
  profileId: string;
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
  /** Extra node properties to include alongside the base seven fields. */
  fields?: string[];
  /** How many child levels to traverse. 0 (default) returns the top-level nodes only. */
  depth?: number;
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
  | ResultMessage
  | SetProfileMessage;

/**
 * Union of messages the plugin main thread can receive.
 * FILE_INFO and RESULT are outbound-only and are excluded.
 */
export type InboundMessage =
  | StatusMessage
  | ExecuteMessage
  | GetSelectionMessage
  | ScreenshotMessage
  | SetProfileMessage;

/**
 * Returns true when `x` is a well-formed inbound message for the plugin main thread.
 * Accepts STATUS, EXECUTE, GET_SELECTION, SCREENSHOT, and SET_PROFILE only.
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
      return typeof msg.requestId === "number" && typeof msg.code === "string";
    case "GET_SELECTION":
      return typeof msg.requestId === "number";
    case "SCREENSHOT":
      return typeof msg.requestId === "number";
    case "SET_PROFILE":
      return typeof msg.profileId === "string";
    default:
      return false;
  }
}

/**
 * Returns true when `x` is a well-formed message for the plugin main thread.
 * Most types arrive from the daemon over the WebSocket. `SET_PROFILE` arrives
 * from the plugin UI iframe. Use this guard before the main thread handles any
 * inbound message.
 */
export function isDaemonMessage(x: unknown): x is DaemonMessage {
  if (typeof x !== "object" || x === null) return false;
  const msg = x as Record<string, unknown>;
  if (typeof msg.type !== "string") return false;
  switch (msg.type) {
    case "FILE_INFO":
      return (
        typeof msg.fileKey === "string" &&
        typeof msg.name === "string" &&
        typeof msg.profileId === "string"
      );
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
    case "SET_PROFILE":
      return typeof msg.profileId === "string";
    default:
      return false;
  }
}

/** Plugin-data key used to persist the active taste profile per Figma file. */
export const PROFILE_KEY = "turbofig:profile";

/**
 * Minimal interface for reading and writing Figma plugin data.
 * Matches the shape of figma.root (and any FrameNode / DocumentNode).
 * Use this in unit tests with a plain in-memory object.
 */
export interface PluginDataStore {
  getPluginData(key: string): string;
  setPluginData(key: string, value: string): void;
}

/**
 * Returns the active taste profile id stored on the Figma document root.
 * Returns an empty string when no profile has been set (Figma returns "" for unset keys).
 */
export function readProfileId(root: PluginDataStore): string {
  return root.getPluginData(PROFILE_KEY);
}

/**
 * Writes the given profile id to the Figma document root.
 * Pass figma.root directly; it satisfies the PluginDataStore shape.
 * An empty string is rejected: the function returns without writing.
 * "none" is a valid non-empty id and is written normally.
 */
export function applySetProfile(root: PluginDataStore, profileId: string): void {
  if (profileId === "") return;
  root.setPluginData(PROFILE_KEY, profileId);
}

/**
 * Builds a FILE_INFO message for the given file key, document name, and profile id.
 * Use this in the plugin main thread to post file identity to the UI.
 */
export function buildFileInfo(fileKey: string, name: string, profileId: string): FileInfoMessage {
  return { type: "FILE_INFO", fileKey, name, profileId };
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
 * Builds a SET_PROFILE message for the given profile id.
 * Post this to the plugin main thread to change the active taste profile.
 * Do not send it over the WebSocket.
 */
export function buildSetProfile(profileId: string): SetProfileMessage {
  return { type: "SET_PROFILE", profileId };
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
      const val = node[field];
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
