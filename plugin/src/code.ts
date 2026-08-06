import { createTf } from "./helpers";
import type {
  ExecuteMessage,
  GetSelectionMessage,
  ResultMessage,
  ScreenshotMessage,
} from "./protocol";
import {
  applySetProfile,
  buildExecuteError,
  buildExecuteSuccess,
  buildFileInfo,
  buildResult,
  buildScreenshot,
  buildSelection,
  isInboundMessage,
  readProfileId,
  safeResult,
  serializeNode,
  wrapUserCode,
} from "./protocol";

/**
 * Handles an EXECUTE message. Runs user code in an async function with figma and tf in scope.
 * Returns a success ResultMessage, or an error ResultMessage when the code throws.
 * Never rejects: a thrown non-Error value (e.g. throw "boom") becomes an error message.
 */
export async function handleExecute(
  figma: PluginAPI,
  tf: ReturnType<typeof createTf>,
  msg: ExecuteMessage,
): Promise<ResultMessage> {
  const { requestId, code } = msg;
  const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
    ...args: string[]
  ) => (...args: unknown[]) => Promise<unknown>;
  try {
    // Build and run the user code in an async function with figma and tf in scope.
    const fn = new AsyncFunction("figma", "tf", wrapUserCode(code));
    const result = await fn(figma, tf);
    return buildExecuteSuccess(requestId, safeResult(result));
  } catch (err) {
    const message =
      err != null && typeof err === "object" && "message" in err
        ? String((err as { message: unknown }).message)
        : String(err);
    return buildExecuteError(requestId, message);
  }
}

/**
 * Handles a GET_SELECTION message. Serialises the current Figma selection.
 * Returns a selection ResultMessage, or an error ResultMessage on failure.
 */
export async function handleGetSelection(
  figma: PluginAPI,
  msg: GetSelectionMessage,
): Promise<ResultMessage> {
  const { requestId, fields, depth } = msg;
  try {
    // Map each selected node with the caller's field/depth options.
    const items = figma.currentPage.selection.map((n) =>
      serializeNode(n as unknown as Record<string, unknown>, fields, depth ?? 0),
    );
    return buildSelection(requestId, items as Parameters<typeof buildSelection>[1]);
  } catch (err) {
    const message =
      err != null && typeof err === "object" && "message" in err
        ? String((err as { message: unknown }).message)
        : String(err);
    return buildExecuteError(requestId, message);
  }
}

/**
 * Handles a SCREENSHOT message. Exports the target node as a PNG.
 * Returns a screenshot ResultMessage, or an error ResultMessage when the node is
 * missing or not exportable.
 */
export async function handleScreenshot(
  figma: PluginAPI,
  msg: ScreenshotMessage,
): Promise<ResultMessage> {
  const { requestId } = msg;
  try {
    // Resolve the target node: prefer nodeId, then selection, else error.
    let node: BaseNode | null = null;
    if (msg.nodeId) {
      node = await figma.getNodeByIdAsync(msg.nodeId);
    } else if (figma.currentPage.selection.length > 0) {
      node = figma.currentPage.selection[0] as unknown as BaseNode;
    } else {
      return buildExecuteError(requestId, "no node to screenshot: select a node or pass nodeId");
    }
    // Guard: node must exist and support exportAsync.
    if (node === null || !("exportAsync" in node)) {
      return buildExecuteError(requestId, "node is not exportable");
    }
    // Safe cast: node has exportAsync, width, height after the guard above.
    const exportable = node as unknown as SceneNode & ExportMixin;
    const scale = typeof msg.scale === "number" && msg.scale > 0 ? msg.scale : 1;
    const bytes = await exportable.exportAsync({
      format: "PNG",
      constraint: { type: "SCALE", value: scale },
    });
    const png = figma.base64Encode(bytes);
    return buildScreenshot(requestId, png, exportable.width, exportable.height);
  } catch (err) {
    const message =
      err != null && typeof err === "object" && "message" in err
        ? String((err as { message: unknown }).message)
        : String(err);
    return buildExecuteError(requestId, message);
  }
}

/** Posts a FILE_INFO message to the UI with the current file identity and profile. */
function emitFileInfo(): void {
  figma.ui.postMessage(
    buildFileInfo(figma.fileKey ?? "", figma.root.name, readProfileId(figma.root)),
  );
}

// Plugin bootstrap. Guard with a globalThis check so this file is importable in unit tests.
// In the Figma sandbox, globalThis.figma is the PluginAPI. Outside it, the block is skipped.
if ((globalThis as Record<string, unknown>).figma !== undefined) {
  figma.showUI(__html__, { width: 320, height: 240 });

  /* Send FILE_INFO to the UI so it can identify the file to the daemon on connect. */
  emitFileInfo();

  figma.ui.onmessage = (msg: unknown) => {
    if (!isInboundMessage(msg)) return;
    switch (msg.type) {
      case "STATUS":
        figma.ui.postMessage(buildResult(msg, figma.fileKey ?? "", figma.root.name));
        break;
      case "EXECUTE": {
        // Build the tf namespace bound to this live PluginAPI instance.
        const tf = createTf(figma);
        void handleExecute(figma, tf, msg).then((reply) => figma.ui.postMessage(reply));
        break;
      }
      case "GET_SELECTION":
        void handleGetSelection(figma, msg).then((reply) => figma.ui.postMessage(reply));
        break;
      case "SCREENSHOT":
        void handleScreenshot(figma, msg).then((reply) => figma.ui.postMessage(reply));
        break;
      case "SET_PROFILE":
        /* Store the new profile id in the document, then re-emit FILE_INFO. */
        applySetProfile(figma.root, msg.profileId);
        emitFileInfo();
        break;
      default:
        break;
    }
  };
}
