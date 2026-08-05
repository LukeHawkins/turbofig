import {
  buildExecuteError,
  buildExecuteSuccess,
  buildResult,
  isDaemonMessage,
  safeResult,
} from "./protocol";

figma.showUI(__html__, { width: 320, height: 240 });

/* Send FILE_INFO to the UI so it can identify the file to the daemon on connect. */
figma.ui.postMessage({
  type: "FILE_INFO",
  fileKey: figma.fileKey ?? "",
  name: figma.root.name,
});

figma.ui.onmessage = (msg: unknown) => {
  if (!isDaemonMessage(msg)) return;
  switch (msg.type) {
    case "STATUS":
      figma.ui.postMessage(buildResult(msg, figma.fileKey ?? "", figma.root.name));
      break;
    case "EXECUTE": {
      const { requestId, code } = msg;
      // TODO: inject deprecation preamble here before evaluating user code.
      const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
        ...args: string[]
      ) => (...args: unknown[]) => Promise<unknown>;
      void (async () => {
        try {
          const fn = new AsyncFunction("figma", code);
          const result = await fn(figma);
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
    default:
      break;
  }
};
