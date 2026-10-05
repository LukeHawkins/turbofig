/**
 * UI runtime for the Turbofig plugin panel.
 * Runs in the browser iframe. Manages the WebSocket connection and panel state.
 * Imports backoffDelayMs from protocol.ts to avoid duplicating the implementation.
 */

import type { FileInfoMessage } from "../protocol";
import { backoffDelayMs, isDaemonMessage } from "../protocol";
import {
  appendLog,
  connStateFromEvent,
  daemonMessageAction,
  formatConnectPrompt,
  formatLogEntry,
  formatSession,
  isRequestType,
  mainMessageAction,
  parsePort,
  portMessageAction,
  screenSize,
  staleWarning,
  wsUrlForPort,
} from "./ui-logic";

/** Build-time constant injected by build-ui.ts via Bun.build define. */
declare const __PLUGIN_VERSION__: string;
/**
 * Build-time constant injected by build-ui.ts. The embedded, checked-in-free
 * `dist/ui.html` carries the literal placeholder string
 * "__TURBOFIG_PAIRING_TOKEN__"; `write_plugin_files` (daemon/src/plugin_files.rs)
 * replaces it with the real token when it writes the plugin out to
 * ~/.turbofig/figma-plugin/. A local `bun run build` injects the real token
 * directly when the daemon has already created ~/.turbofig/token.
 */
declare const __TURBOFIG_PAIRING_TOKEN__: string;

/** Active daemon port. Updated when PORT arrives from the main thread. */
let currentPort = 18847;

/** MCP HTTP port received from the daemon on WELCOME. Default matches TURBOFIG_MCP_PORT default. */
let daemonMcpPort = 18846;

/** File-bridge home directory received from the daemon on WELCOME. Empty until the first WELCOME arrives; formatConnectPrompt falls back to DEFAULT_BRIDGE_HOME. */
let daemonBridgeHome = "";

// Resolve panel elements once on load.
const connStatusEl = document.getElementById("conn-status") as HTMLElement;
const sessionLineEl = document.getElementById("session-line") as HTMLElement;
const sessionRowEl = document.getElementById("session-row") as HTMLElement;
const pluginVersionEl = document.getElementById("plugin-version") as HTMLElement;
const staleWarningEl = document.getElementById("stale-warning") as HTMLElement;
const activityLogEl = document.getElementById("activity-log") as HTMLElement;
const portFieldEl = document.getElementById("port-field") as HTMLInputElement;
const portHintEl = document.getElementById("port-hint") as HTMLElement;
const copyFilekeyBtn = document.getElementById("copy-filekey") as HTMLButtonElement;
const copyConnectBtn = document.getElementById("copy-connect") as HTMLButtonElement;
const navMoreBtn = document.getElementById("nav-more") as HTMLButtonElement;
const navAboutBtn = document.getElementById("nav-about") as HTMLButtonElement;
const navBackBtn = document.getElementById("nav-back") as HTMLButtonElement;
const navBackAboutBtn = document.getElementById("nav-back-about") as HTMLButtonElement;

/** Maximum number of entries kept in the activity log. */
const LOG_CAP = 20;

/** SVG markup for the check icon used as copied-state feedback. */
const CHECK_SVG = `<svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden="true"><path d="M2.5 7l3.5 3.5 5.5-5.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

// Runtime state.
let latestFileInfo: FileInfoMessage | null = null;
let attempt = 0;
let ws: WebSocket | null = null;
/** Pending reconnect timer. Cleared before any new connect so only one runs. */
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
/** Daemon version received on WELCOME. Used for the stale-version check. */
let daemonVersion = "";
let activeSessionId = "";
let activityLog: string[] = [];
/** True once the first PORT (the reply to READY) has arrived and connect() has run. */
let hasConnectedOnce = false;

const CONN_LABELS: Record<string, string> = {
  connected: "Connected to daemon",
  offline: "No connection to daemon",
  connecting: "Connecting...",
  reconnecting: "Reconnecting...",
};

/** Updates the connection status element from a WS lifecycle event. */
function setConnStatus(event: "open" | "close" | "error" | "attempt"): void {
  const { state } = connStateFromEvent(event, attempt);
  if (connStatusEl) {
    connStatusEl.textContent = CONN_LABELS[state] ?? state;
    connStatusEl.className = state;
  }
}

/** Refreshes the copy buttons from the latest FILE_INFO. */
function updateFileDisplay(): void {
  const hasKey = Boolean(latestFileInfo?.fileKey);
  if (copyFilekeyBtn) copyFilekeyBtn.style.display = hasKey ? "" : "none";
  if (copyConnectBtn) copyConnectBtn.style.display = hasKey ? "" : "none";
}

/** Refreshes the session display from the latest active session id. */
function updateSessionDisplay(): void {
  if (!sessionRowEl || !sessionLineEl) return;
  if (!activeSessionId) {
    sessionRowEl.style.display = "none";
  } else {
    sessionRowEl.style.display = "";
    sessionLineEl.textContent = formatSession(activeSessionId);
  }
}

/**
 * Appends one entry to the activity log container and scrolls to it.
 * Appends only the new row instead of clearing and rebuilding the whole
 * container, so the aria-live region announces just the new entry: a full
 * rebuild on every event made a screen reader re-read up to LOG_CAP entries
 * each time. Trims old rows from the top once the log is over LOG_CAP, to
 * match the trimmed `activityLog` array.
 */
function appendActivityLogEntry(entry: string): void {
  if (!activityLogEl) return;
  const row = document.createElement("div");
  row.className = "log-entry";
  row.textContent = entry;
  activityLogEl.appendChild(row);
  while (activityLogEl.childElementCount > LOG_CAP) {
    activityLogEl.firstElementChild?.remove();
  }
  activityLogEl.scrollTop = activityLogEl.scrollHeight;
}

/**
 * Copies text to the clipboard using a hidden textarea.
 * Returns true when the copy succeeded, false otherwise.
 * navigator.clipboard is unreliable inside the Figma plugin iframe.
 */
function clipboardCopy(text: string): boolean {
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.left = "-9999px";
  ta.style.top = "-9999px";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  let success = false;
  try {
    success = document.execCommand("copy");
  } catch {
    success = false;
  }
  document.body.removeChild(ta);
  return success;
}

/** Applies a transient copied-state to a button, showing a check icon for 1.2 seconds. */
function setCopiedFeedback(btn: HTMLButtonElement): void {
  btn.classList.add("copied");
  const orig = btn.innerHTML;
  btn.innerHTML = CHECK_SVG;
  setTimeout(() => {
    btn.classList.remove("copied");
    btn.innerHTML = orig;
  }, 1200);
}

/** Shows the named screen and posts a RESIZE message to resize the plugin panel. */
function showScreen(name: "main" | "advanced" | "about"): void {
  Array.from(document.querySelectorAll<HTMLElement>(".screen")).forEach((s) => {
    s.classList.remove("active");
  });
  const target = document.getElementById(`screen-${name}`);
  if (target) target.classList.add("active");
  const { width, height } = screenSize(name);
  parent.postMessage({ pluginMessage: { type: "RESIZE", width, height } }, "*");
}

/* Copy the full fileKey to the clipboard on click. Show a check icon for brief feedback. */
if (copyFilekeyBtn) {
  copyFilekeyBtn.addEventListener("click", () => {
    // A second click while the check icon is still showing must not capture
    // that icon as the "original" to restore: it would freeze on the check.
    if (copyFilekeyBtn.classList.contains("copied")) return;
    const key = latestFileInfo?.fileKey;
    if (!key) return;
    if (clipboardCopy(key)) {
      setCopiedFeedback(copyFilekeyBtn);
    }
  });
}

/* Copy the connect prompt to the clipboard on click. Show a check icon for brief feedback. */
if (copyConnectBtn) {
  copyConnectBtn.addEventListener("click", () => {
    if (copyConnectBtn.classList.contains("copied")) return;
    if (!latestFileInfo) return;
    const prompt = formatConnectPrompt(latestFileInfo.fileKey, daemonMcpPort, daemonBridgeHome);
    if (!prompt) return;
    if (clipboardCopy(prompt)) {
      setCopiedFeedback(copyConnectBtn);
    }
  });
}

/* Navigate to the advanced screen on More click. */
if (navMoreBtn) {
  navMoreBtn.addEventListener("click", () => showScreen("advanced"));
}

/* Navigate to the about screen on About click. */
if (navAboutBtn) {
  navAboutBtn.addEventListener("click", () => showScreen("about"));
}

/* Navigate back to main from the advanced screen. */
if (navBackBtn) {
  navBackBtn.addEventListener("click", () => showScreen("main"));
}

/* Navigate back to main from the about screen. */
if (navBackAboutBtn) {
  navBackAboutBtn.addEventListener("click", () => showScreen("main"));
}

/* Commit the port on change (blur or Enter).
   Invalid input shows the hint and resets the field to the last valid port. */
portFieldEl.onchange = () => {
  const parsed = parsePort(portFieldEl.value);
  if (parsed === null) {
    if (portHintEl) portHintEl.style.display = "block";
    portFieldEl.value = String(currentPort);
    return;
  }
  if (portHintEl) portHintEl.style.display = "none";
  parent.postMessage({ pluginMessage: { type: "SET_PORT", port: parsed } }, "*");
};

/** Opens a WebSocket and registers lifecycle handlers. */
function connect(): void {
  /* Cancel any pending reconnect so a stale timer cannot open a second socket. */
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  setConnStatus("attempt");
  const socket = new WebSocket(wsUrlForPort(currentPort, __TURBOFIG_PAIRING_TOKEN__));
  ws = socket;

  socket.onopen = () => {
    attempt = 0;
    setConnStatus("open");
    /* Re-identify the file to the daemon on every successful connect. */
    if (latestFileInfo) {
      socket.send(JSON.stringify(latestFileInfo));
    }
  };

  socket.onclose = () => {
    scheduleReconnect();
  };

  socket.onerror = () => {
    /* onclose fires after onerror; let onclose drive the reconnect. */
  };

  /* Forward daemon request messages to the main thread. */
  socket.onmessage = (event: MessageEvent) => {
    let candidate: unknown;
    try {
      candidate = JSON.parse(event.data as string);
    } catch {
      return;
    }
    // A JSON `null`, a number, or a malformed payload would otherwise throw
    // on `.type` below. isDaemonMessage rejects anything that is not one of
    // the known daemon-to-plugin message shapes.
    if (!isDaemonMessage(candidate)) return;
    const parsed = candidate as unknown as Record<string, unknown>;
    const action = daemonMessageAction(parsed.type as string);
    /* Consume WELCOME: store the daemon version and MCP port; check for a stale mismatch. */
    if (action === "welcome") {
      daemonVersion = typeof parsed.version === "string" ? parsed.version : "";
      daemonMcpPort = typeof parsed.mcpPort === "number" ? parsed.mcpPort : 18846;
      daemonBridgeHome = typeof parsed.bridgeHome === "string" ? parsed.bridgeHome : "";
      const warning = staleWarning(__PLUGIN_VERSION__, daemonVersion);
      if (staleWarningEl) {
        staleWarningEl.textContent = warning;
        staleWarningEl.style.display = warning ? "block" : "none";
      }
      return;
    }
    /* Read the active session id from a request that carries a real one.
       A file-bridge call sends an empty session id. Keep the last real
       session so a bridge call does not clear the session display. */
    if (typeof parsed.sessionId === "string" && parsed.sessionId !== "") {
      activeSessionId = parsed.sessionId;
      updateSessionDisplay();
    }
    /* Append request types to the activity log. activityLog is the source of
       truth for the cap; the DOM append is purely incremental (see
       appendActivityLogEntry) so aria-live announces only the new entry. */
    if (isRequestType(parsed.type as string)) {
      activityLog = appendLog(
        activityLog,
        formatLogEntry(parsed.type as string, Date.now()),
        LOG_CAP,
      );
      appendActivityLogEntry(activityLog[activityLog.length - 1] as string);
    }
    parent.postMessage({ pluginMessage: parsed }, "*");
  };
}

/** Increments the attempt counter and schedules a reconnect after the backoff delay. */
function scheduleReconnect(): void {
  const delay = backoffDelayMs(attempt);
  attempt += 1;
  setConnStatus("close");
  reconnectTimer = setTimeout(connect, delay);
}

/* Forward main-thread messages over the WS. FILE_INFO is stored and re-sent on each connect. */
window.onmessage = (event: MessageEvent) => {
  const data = event.data as { pluginMessage?: unknown } | null;
  const msg = data?.pluginMessage;
  if (!msg || typeof msg !== "object") return;
  const m = msg as Record<string, unknown>;
  const action = mainMessageAction(m.type as string);
  /* PORT: update the port field, then connect (first arrival) or reconnect
     (port changed after an earlier connect). PORT is a reply to the READY
     handshake sent on load, so this is also the first connect. */
  if (action === "port") {
    const newPort = typeof m.port === "number" ? m.port : 18847;
    if (portFieldEl) portFieldEl.value = String(newPort);
    const next = portMessageAction(hasConnectedOnce, currentPort, newPort);
    if (next === "connect") {
      hasConnectedOnce = true;
      currentPort = newPort;
      connect();
    } else if (next === "reconnect") {
      currentPort = newPort;
      /* Detach onclose before closing so the old socket does not schedule a reconnect. */
      if (ws) {
        ws.onclose = null;
        ws.close();
      }
      attempt = 0;
      /* connect() clears any pending reconnect timer, so only one socket opens. */
      connect();
    }
    return;
  }
  if (action === "fileinfo") {
    latestFileInfo = m as unknown as FileInfoMessage;
    updateFileDisplay();
    /* If already connected, send FILE_INFO now so it reaches the daemon. */
    if (ws && ws.readyState === 1 /* OPEN */) {
      ws.send(JSON.stringify(m));
    }
    return;
  }
  /* Relay RESULT and all other main-thread messages over the WS. */
  if (ws && ws.readyState === 1 /* OPEN */) {
    ws.send(JSON.stringify(m));
  }
};

// Show the plugin version immediately on load.
if (pluginVersionEl) pluginVersionEl.textContent = __PLUGIN_VERSION__;

/* Tell the main thread the UI is ready. It replies with FILE_INFO and PORT;
   the PORT handler above makes the first connect() call once that arrives.
   This handshake means the UI can never connect with a stale default port
   nor miss the file identity (see createDispatcher in code.ts). */
parent.postMessage({ pluginMessage: { type: "READY" } }, "*");
parent.postMessage(
  { pluginMessage: { type: "RESIZE", width: 300, height: screenSize("main").height } },
  "*",
);
