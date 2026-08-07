/**
 * UI runtime for the Turbofig plugin panel.
 * Runs in the browser iframe. Manages the WebSocket connection and panel state.
 * Imports backoffDelayMs from protocol.ts to avoid duplicating the implementation.
 */

import type { FileInfoMessage } from "../protocol";
import { backoffDelayMs } from "../protocol";
import {
  appendLog,
  connStateFromEvent,
  daemonMessageAction,
  formatConnectPrompt,
  formatFileLine,
  formatLogEntry,
  formatSession,
  isRequestType,
  mainMessageAction,
  parsePort,
  profileToSelectValue,
  screenSize,
  staleWarning,
  wsUrlForPort,
} from "./ui-logic";

/** Build-time constant injected by build-ui.ts via Bun.build define. */
declare const __PLUGIN_VERSION__: string;

/** Active daemon port. Updated when PORT arrives from the main thread. */
let currentPort = 18847;

/** MCP HTTP port received from the daemon on WELCOME. Default matches TURBOFIG_MCP_PORT default. */
let daemonMcpPort = 18846;

// Resolve panel elements once on load.
const connStatusEl = document.getElementById("conn-status") as HTMLElement;
const fileLineEl = document.getElementById("file-line") as HTMLElement;
const sessionLineEl = document.getElementById("session-line") as HTMLElement;
const sessionRowEl = document.getElementById("session-row") as HTMLElement;
const pluginVersionEl = document.getElementById("plugin-version") as HTMLElement;
const staleWarningEl = document.getElementById("stale-warning") as HTMLElement;
const activityLogEl = document.getElementById("activity-log") as HTMLElement;
const profileSel = document.getElementById("profile") as HTMLSelectElement;
const customInput = document.getElementById("customProfile") as HTMLInputElement;
const portFieldEl = document.getElementById("port-field") as HTMLInputElement;
const portHintEl = document.getElementById("port-hint") as HTMLElement;
const copyFilekeyBtn = document.getElementById("copy-filekey") as HTMLButtonElement;
const copyConnectBtn = document.getElementById("copy-connect") as HTMLButtonElement;
const navAdvancedBtn = document.getElementById("nav-advanced") as HTMLButtonElement;
const navBackBtn = document.getElementById("nav-back") as HTMLButtonElement;

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

/** Updates the connection status element from a WS lifecycle event. */
function setConnStatus(event: "open" | "close" | "error" | "attempt"): void {
  const { state, label } = connStateFromEvent(event, attempt);
  if (connStatusEl) {
    connStatusEl.textContent = label;
    connStatusEl.className = state;
  }
}

/** Refreshes the file display from the latest FILE_INFO. */
function updateFileDisplay(): void {
  if (!fileLineEl) return;
  if (latestFileInfo) {
    fileLineEl.textContent = formatFileLine(latestFileInfo.fileKey, latestFileInfo.name);
    const hasKey = Boolean(latestFileInfo.fileKey);
    if (copyFilekeyBtn) copyFilekeyBtn.style.display = hasKey ? "" : "none";
    if (copyConnectBtn) copyConnectBtn.style.display = hasKey ? "" : "none";
  } else {
    fileLineEl.textContent = "No file";
    if (copyFilekeyBtn) copyFilekeyBtn.style.display = "none";
    if (copyConnectBtn) copyConnectBtn.style.display = "none";
  }
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

/** Re-renders the activity log container and scrolls to the newest entry at the bottom. */
function renderActivityLog(): void {
  if (!activityLogEl) return;
  activityLogEl.textContent = "";
  for (const entry of activityLog) {
    const row = document.createElement("div");
    row.className = "log-entry";
    row.textContent = entry;
    activityLogEl.appendChild(row);
  }
  activityLogEl.scrollTop = activityLogEl.scrollHeight;
}

/** Returns the current profile id selected in the panel. */
function getSelectedProfileId(): string {
  if (profileSel.value === "custom") {
    return customInput.value.trim();
  }
  return profileSel.value;
}

/** Posts SET_PROFILE to the main thread with the current selector value. */
function postSetProfile(): void {
  parent.postMessage(
    { pluginMessage: { type: "SET_PROFILE", profileId: getSelectedProfileId() } },
    "*",
  );
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
function showScreen(name: "main" | "advanced"): void {
  Array.from(document.querySelectorAll<HTMLElement>(".screen")).forEach((s) => {
    s.classList.remove("active");
  });
  const target = document.getElementById(`screen-${name}`);
  if (target) target.classList.add("active");
  const { width, height } = screenSize(name);
  parent.postMessage({ pluginMessage: { type: "RESIZE", width, height } }, "*");
}

profileSel.onchange = () => {
  if (profileSel.value === "custom") {
    /* Show the custom field and wait for the user to commit an id.
       Do not post yet: an empty custom id resolves to impeccable and
       would snap the select off "custom" while the user is typing. */
    customInput.style.display = "block";
    customInput.focus();
    return;
  }
  customInput.style.display = "none";
  postSetProfile();
};

/* Commit the custom id on change (blur or Enter), not on each keystroke.
   This stops a mid-typing FILE_INFO echo from reformatting the control. */
customInput.onchange = () => {
  if (profileSel.value === "custom" && customInput.value.trim() !== "") {
    postSetProfile();
  }
};

/* Copy the full fileKey to the clipboard on click. Show a check icon for brief feedback. */
if (copyFilekeyBtn) {
  copyFilekeyBtn.addEventListener("click", () => {
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
    if (!latestFileInfo) return;
    const prompt = formatConnectPrompt(latestFileInfo.fileKey, daemonMcpPort);
    if (!prompt) return;
    if (clipboardCopy(prompt)) {
      setCopiedFeedback(copyConnectBtn);
    }
  });
}

/* Navigate to the advanced screen on gear button click. */
if (navAdvancedBtn) {
  navAdvancedBtn.addEventListener("click", () => showScreen("advanced"));
}

/* Navigate back to the main screen on back button click. */
if (navBackBtn) {
  navBackBtn.addEventListener("click", () => showScreen("main"));
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
  const socket = new WebSocket(wsUrlForPort(currentPort));
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
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(event.data as string) as Record<string, unknown>;
    } catch {
      return;
    }
    const action = daemonMessageAction(parsed.type as string);
    /* The UI selector owns SET_PROFILE. Drop it if the daemon sends it. */
    if (action === "drop") return;
    /* Consume WELCOME: store the daemon version and MCP port; check for a stale mismatch. */
    if (action === "welcome") {
      daemonVersion = typeof parsed.version === "string" ? parsed.version : "";
      daemonMcpPort = typeof parsed.mcpPort === "number" ? parsed.mcpPort : 18846;
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
    /* Append request types to the activity log. */
    if (isRequestType(parsed.type as string)) {
      activityLog = appendLog(
        activityLog,
        formatLogEntry(parsed.type as string, Date.now()),
        LOG_CAP,
      );
      renderActivityLog();
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
  /* PORT: update the port field and reconnect when the port changes. */
  if (action === "port") {
    const newPort = typeof m.port === "number" ? m.port : 18847;
    if (portFieldEl) portFieldEl.value = String(newPort);
    if (newPort !== currentPort) {
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
    /* Reflect the current profile in the selector. Default to impeccable. */
    const pid = typeof m.profileId === "string" && m.profileId !== "" ? m.profileId : "impeccable";
    const { value, isCustom } = profileToSelectValue(pid);
    profileSel.value = value;
    if (isCustom) {
      customInput.value = typeof m.profileId === "string" ? m.profileId : "";
      customInput.style.display = "block";
    } else {
      customInput.style.display = "none";
    }
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

connect();
