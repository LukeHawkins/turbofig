import { buildResult, isDaemonMessage } from "./protocol";

figma.showUI(__html__, { width: 320, height: 240 });

/* Send FILE_INFO to the UI so it can identify the file to the daemon on connect. */
figma.ui.postMessage({
  type: "FILE_INFO",
  fileKey: figma.fileKey ?? "",
  name: figma.root.name,
});

figma.ui.onmessage = (msg: unknown) => {
  if (!isDaemonMessage(msg)) return;
  if (msg.type === "STATUS") {
    figma.ui.postMessage(buildResult(msg, figma.fileKey ?? "", figma.root.name));
  }
};
