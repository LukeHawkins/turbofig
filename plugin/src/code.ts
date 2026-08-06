import { createTf } from "./helpers";
import {
  applySetProfile,
  buildExecuteError,
  buildExecuteSuccess,
  buildFileInfo,
  buildResult,
  buildScreenshot,
  buildSelection,
  isDaemonMessage,
  readProfileId,
  safeResult,
  serializeNode,
  wrapUserCode,
} from "./protocol";

figma.showUI(__html__, { width: 320, height: 240 });

/** Posts a FILE_INFO message to the UI with the current file identity and profile. */
function emitFileInfo(): void {
  figma.ui.postMessage(
    buildFileInfo(figma.fileKey ?? "", figma.root.name, readProfileId(figma.root)),
  );
}

/* Send FILE_INFO to the UI so it can identify the file to the daemon on connect. */
emitFileInfo();

figma.ui.onmessage = (msg: unknown) => {
  if (!isDaemonMessage(msg)) return;
  switch (msg.type) {
    case "STATUS":
      figma.ui.postMessage(buildResult(msg, figma.fileKey ?? "", figma.root.name));
      break;
    case "EXECUTE": {
      const { requestId, code } = msg;
      const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
        ...args: string[]
      ) => (...args: unknown[]) => Promise<unknown>;
      void (async () => {
        try {
          // Build the tf namespace bound to this live PluginAPI instance.
          const tf = createTf(figma);
          // The preamble runs first. It sets async APIs under dynamic-page.
          const fn = new AsyncFunction("figma", "tf", wrapUserCode(code));
          const result = await fn(figma, tf);
          figma.ui.postMessage(buildExecuteSuccess(requestId, safeResult(result)));
        } catch (err) {
          const message =
            err != null && typeof err === "object" && "message" in err
              ? String((err as { message: unknown }).message)
              : String(err);
          figma.ui.postMessage(buildExecuteError(requestId, message));
        }
      })();
      break;
    }
    case "GET_SELECTION": {
      const { requestId, fields, depth } = msg;
      try {
        // Map each selected node with the caller's field/depth options.
        const items = figma.currentPage.selection.map((n) =>
          serializeNode(n as unknown as Record<string, unknown>, fields, depth ?? 0),
        );
        figma.ui.postMessage(
          buildSelection(requestId, items as Parameters<typeof buildSelection>[1]),
        );
      } catch (err) {
        const message =
          err != null && typeof err === "object" && "message" in err
            ? String((err as { message: unknown }).message)
            : String(err);
        figma.ui.postMessage(buildExecuteError(requestId, message));
      }
      break;
    }
    case "SCREENSHOT": {
      const { requestId } = msg;
      void (async () => {
        try {
          // Resolve the target node: prefer nodeId, then selection, else error.
          let node: BaseNode | null = null;
          if (msg.nodeId) {
            node = await figma.getNodeByIdAsync(msg.nodeId);
          } else if (figma.currentPage.selection.length > 0) {
            node = figma.currentPage.selection[0] as unknown as BaseNode;
          } else {
            figma.ui.postMessage(
              buildExecuteError(requestId, "no node to screenshot: select a node or pass nodeId"),
            );
            return;
          }
          // Guard: node must exist and support exportAsync.
          if (node === null || !("exportAsync" in node)) {
            figma.ui.postMessage(buildExecuteError(requestId, "node is not exportable"));
            return;
          }
          // Safe cast: node has exportAsync, width, height after the guard above.
          const exportable = node as unknown as SceneNode & ExportMixin;
          const scale = typeof msg.scale === "number" && msg.scale > 0 ? msg.scale : 1;
          const bytes = await exportable.exportAsync({
            format: "PNG",
            constraint: { type: "SCALE", value: scale },
          });
          const png = figma.base64Encode(bytes);
          figma.ui.postMessage(
            buildScreenshot(requestId, png, exportable.width, exportable.height),
          );
        } catch (err) {
          const message =
            err != null && typeof err === "object" && "message" in err
              ? String((err as { message: unknown }).message)
              : String(err);
          figma.ui.postMessage(buildExecuteError(requestId, message));
        }
      })();
      break;
    }
    case "SET_PROFILE":
      /* Store the new profile id in the document, then re-emit FILE_INFO. */
      applySetProfile(figma.root, msg.profileId);
      emitFileInfo();
      break;
    default:
      break;
  }
};
