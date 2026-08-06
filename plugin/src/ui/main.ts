/**
 * UI runtime for the Turbofig plugin panel.
 * Runs in the browser iframe. Manages the WebSocket connection and panel state.
 * Imports backoffDelayMs from protocol.ts to avoid duplicating the implementation.
 */

import type { FileInfoMessage } from "../protocol";
import { backoffDelayMs } from "../protocol";
import {
  connStateFromEvent,
  formatFileLine,
  formatPairing,
  formatSession,
  profileToSelectValue,
} from "./ui-logic";

const WS_URL = "ws://localhost:18847";

// Resolve panel elements once on load.
const connStatusEl = document.getElementById("conn-status") as HTMLElement;
const fileLineEl = document.getElementById("file-line") as HTMLElement;
const sessionLineEl = document.getElementById("session-line") as HTMLElement;
const pairingLineEl = document.getElementById("pairing-line") as HTMLElement;
const profileSel = document.getElementById("profile") as HTMLSelectElement;
const customInput = document.getElementById("customProfile") as HTMLInputElement;

// Runtime state.
let latestFileInfo: FileInfoMessage | null = null;
let attempt = 0;
let ws: WebSocket | null = null;
/** Daemon version received on WELCOME. Reserved for a future stale-check feature. */
let _daemonVersion = "";
let activeSessionId = "";

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
  } else {
    fileLineEl.textContent = "No file";
  }
}

/** Refreshes the session display from the latest active session id. */
function updateSessionDisplay(): void {
  if (sessionLineEl) {
    sessionLineEl.textContent = formatSession(activeSessionId);
  }
}

/** Refreshes the pairing display from the active session id and file name. */
function updatePairingDisplay(): void {
  if (!pairingLineEl) return;
  const fileName = latestFileInfo ? latestFileInfo.name : "";
  const { paired, label } = formatPairing(activeSessionId, fileName);
  pairingLineEl.textContent = label;
  pairingLineEl.className = paired ? "paired" : "unpaired";
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

/** Opens a WebSocket and registers lifecycle handlers. */
function connect(): void {
  setConnStatus("attempt");
  const socket = new WebSocket(WS_URL);
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
    /* The UI selector owns SET_PROFILE. Drop it if the daemon sends it. */
    if (parsed.type === "SET_PROFILE") return;
    /* Consume WELCOME: store the daemon version. Do not forward to main thread. */
    if (parsed.type === "WELCOME") {
      _daemonVersion = typeof parsed.version === "string" ? parsed.version : "";
      return;
    }
    /* Read the active session id from any request message that carries it. */
    if (typeof parsed.sessionId === "string") {
      activeSessionId = parsed.sessionId;
      updateSessionDisplay();
      updatePairingDisplay();
    }
    parent.postMessage({ pluginMessage: parsed }, "*");
  };
}

/** Increments the attempt counter and schedules a reconnect after the backoff delay. */
function scheduleReconnect(): void {
  const delay = backoffDelayMs(attempt);
  attempt += 1;
  setConnStatus("close");
  setTimeout(connect, delay);
}

/* Forward main-thread messages over the WS. FILE_INFO is stored and re-sent on each connect. */
window.onmessage = (event: MessageEvent) => {
  const data = event.data as { pluginMessage?: unknown } | null;
  const msg = data?.pluginMessage;
  if (!msg || typeof msg !== "object") return;
  const m = msg as Record<string, unknown>;
  if (m.type === "FILE_INFO") {
    latestFileInfo = m as unknown as FileInfoMessage;
    updateFileDisplay();
    updatePairingDisplay();
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

connect();
