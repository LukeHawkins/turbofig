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

/** Minimal subset of figma.clientStorage needed by applySetPort. */
interface ClientStorage {
  getAsync(key: string): Promise<unknown>;
  setAsync(key: string, value: unknown): Promise<void>;
}

/**
 * Validates and persists a daemon WebSocket port.
 * Returns the saved port when valid (integer, 1–65535), or null when invalid.
 * Extracted for testability without a live Figma environment.
 */
export async function applySetPort(storage: ClientStorage, port: number): Promise<number | null> {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  await storage.setAsync("turbofig:wsPort", port);
  return port;
}

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
  // themeColors makes Figma inject the --figma-color-* variables and a
  // figma-light/figma-dark class, so the panel matches the user's theme.
  figma.showUI(__html__, { width: 300, height: 200, themeColors: true });

  /* Send FILE_INFO to the UI so it can identify the file to the daemon on connect. */
  emitFileInfo();

  /* Read the stored port and send it to the UI. The UI connects after receiving PORT. */
  void (async () => {
    const stored = await figma.clientStorage.getAsync("turbofig:wsPort");
    const port =
      typeof stored === "number" && Number.isInteger(stored) && stored >= 1 && stored <= 65535
        ? stored
        : 18847;
    figma.ui.postMessage({ type: "PORT", port });
  })();

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
      case "SET_PORT":
        /* Validate and persist the port; send PORT back so the UI can reconnect. */
        void applySetPort(figma.clientStorage, msg.port).then((saved) => {
          if (saved !== null) {
            figma.ui.postMessage({ type: "PORT", port: saved });
          }
        });
        break;
      case "RESIZE":
        /* Resize the plugin panel to the dimensions requested by the UI. */
        if (msg.width > 0 && msg.height > 0) {
          figma.ui.resize(msg.width, msg.height);
        }
        break;
      default:
        break;
    }
  };
}
