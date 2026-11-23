/**
 * UI runtime for the turbofig plugin panel.
 * Runs in the browser iframe. Manages the WebSocket connection and panel state.
 * Imports backoffDelayMs from protocol.ts to avoid duplicating the implementation.
 */

import type { FileInfoMessage } from "../protocol";
import { backoffDelayMs, isDaemonMessage } from "../protocol";
import { createLiveness } from "./liveness";
import {
  appendLog,
  buildOpenUrlMessage,
  buildSocketUrl,
  connStateFromEvent,
  daemonMessageAction,
  decideFrame,
  fillAgentPrompt,
  formatLogEntry,
  formatSession,
  generateNonce,
  hasPairingToken,
  healthAfterClose,
  INITIAL_LINK_HEALTH,
  isRequestType,
  type LinkHealth,
  mainMessageAction,
  NO_TOKEN_LABEL,
  type PromptMode,
  parsePort,
  parsePromptMode,
  parseShowLog,
  portMessageAction,
  screenSize,
  shouldRenderLog,
  staleWarning,
  statusOverride,
  trackActiveJobs,
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
/**
 * Build-time constant injected by build-ui.ts: the shared agent-connect-
 * prompt template (`prompts/agent-prompt.txt`), read from disk at build time
 * so this bundle and the daemon's menu bar (`include_str!` on the same file)
 * can never drift apart. See `fillAgentPrompt` in `ui-logic.ts`.
 */
declare const __AGENT_PROMPT_TEMPLATE__: string;
/** Build-time constant: the MCP variant (`prompts/agent-prompt-mcp.txt`). */
declare const __AGENT_PROMPT_MCP_TEMPLATE__: string;

/** Chosen "Copy prompt for" mode. Updated when PROMPT_MODE arrives and on user choice. */
let promptMode: PromptMode = "file";

/** "Activity log" setting. Updated when SHOW_LOG arrives and on user choice. */
let showLog = true;
/** Name of the screen on show now. */
let currentScreen: "main" | "advanced" | "about" = "main";

/** Request ids of jobs the plugin is running now (STARTED seen, RESULT not yet). */
let activeJobs: ReadonlySet<number> = new Set();

/** Active daemon port. Updated when PORT arrives from the main thread. */
let currentPort = 18847;

/** MCP HTTP port received from the daemon on WELCOME. Default matches TURBOFIG_MCP_PORT default. */
let daemonMcpPort = 18846;

/** File-bridge home directory received from the daemon on WELCOME. Empty until the first WELCOME arrives; fillAgentPrompt falls back to DEFAULT_BRIDGE_HOME. */
let daemonBridgeHome = "";

/** Daemon binary path received on WELCOME. Empty for an older daemon; fillAgentPrompt then names plain `turbofig`. */
let daemonBinPath = "";

/** Absolute custom bridge folder from WELCOME. Empty for the default folder or an older daemon. */
let daemonBridgeDirCustom = "";

// Resolve panel elements once on load.
const connStatusEl = document.getElementById("conn-status") as HTMLElement;
const sessionLineEl = document.getElementById("session-line") as HTMLElement;
const sessionRowEl = document.getElementById("session-row") as HTMLElement;
const pluginVersionEl = document.getElementById("plugin-version") as HTMLElement;
const staleWarningEl = document.getElementById("stale-warning") as HTMLElement;
const activityLogEl = document.getElementById("activity-log") as HTMLElement;
const portFieldEl = document.getElementById("port-field") as HTMLInputElement;
const portHintEl = document.getElementById("port-hint") as HTMLElement;
const promptModeEls = Array.from(
  document.querySelectorAll<HTMLInputElement>('input[name="prompt-mode"]'),
);
const showLogEl = document.getElementById("show-log") as HTMLInputElement;
const logSectionEl = document.getElementById("log-section") as HTMLElement;
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
  offline: "Daemon not running. Run `turbofig` in Terminal.",
  connecting: "Connecting...",
  reconnecting: "Reconnecting...",
};

const activityEl = document.getElementById("activity");
let fadeTimer: ReturnType<typeof setTimeout> | undefined;

/** How long the sweep keeps running after the last job, so the fade-out is smooth (ms). */
const ACTIVITY_FADE_MS = 500;

/** Shows the activity squares while at least 1 job runs. CSS block: "ACTIVITY SQUARES". */
function setWorking(): void {
  const working = activeJobs.size > 0;
  document.body.classList.toggle("working", working);
  if (!activityEl) return;
  if (fadeTimer !== undefined) clearTimeout(fadeTimer);
  fadeTimer = undefined;
  if (working) {
    activityEl.classList.remove("fading");
    activityEl.classList.add("on");
  } else if (activityEl.classList.contains("on")) {
    activityEl.classList.remove("on");
    activityEl.classList.add("fading");
    fadeTimer = setTimeout(() => activityEl.classList.remove("fading"), ACTIVITY_FADE_MS);
  }
}

/* External links: ask the main thread to open them (figma.openExternal).
   A plain link does nothing in the Figma iframe. */
for (const el of Array.from(document.querySelectorAll<HTMLElement>("[data-open-url]"))) {
  el.addEventListener("click", () => {
    parent.postMessage({ pluginMessage: buildOpenUrlMessage(el.dataset.openUrl ?? "") }, "*");
  });
}

/** Checks the radio that matches the prompt mode. */
function showPromptMode(): void {
  for (const el of promptModeEls) el.checked = el.value === promptMode;
}

/** Shows or hides the activity log section and fits the advanced panel to it. */
function showLogSetting(): void {
  if (showLogEl) showLogEl.checked = showLog;
  if (logSectionEl) logSectionEl.hidden = !showLog;
  if (currentScreen === "advanced") {
    const { width, height } = screenSize("advanced", showLog);
    parent.postMessage({ pluginMessage: { type: "RESIZE", width, height } }, "*");
  }
}

/** Updates the connection status element from a WS lifecycle event. */
/** Handshake health across reconnects (sticky status text). */
let linkHealth: LinkHealth = INITIAL_LINK_HEALTH;

function setConnStatus(event: "open" | "close" | "error" | "attempt"): void {
  const { state } = connStateFromEvent(event, attempt);
  if (connStatusEl) {
    const override = event === "open" ? null : statusOverride(linkHealth);
    connStatusEl.textContent = override ?? CONN_LABELS[state] ?? state;
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
  if (!activityLogEl || !shouldRenderLog(showLog)) return;
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
  currentScreen = name;
  const { width, height } = screenSize(name, showLog);
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
    if (!latestFileInfo?.fileKey) return;
    const prompt = fillAgentPrompt(
      promptMode === "mcp" ? __AGENT_PROMPT_MCP_TEMPLATE__ : __AGENT_PROMPT_TEMPLATE__,
      [{ fileKey: latestFileInfo.fileKey, name: latestFileInfo.name }],
      daemonBridgeHome,
      daemonMcpPort,
      daemonBinPath,
      daemonBridgeDirCustom,
    );
    if (clipboardCopy(prompt)) {
      setCopiedFeedback(copyConnectBtn);
    }
  });
}

/* Persist the chosen prompt mode through the main thread. */
for (const el of promptModeEls) {
  el.addEventListener("change", () => {
    if (!el.checked) return;
    promptMode = parsePromptMode(el.value);
    parent.postMessage({ pluginMessage: { type: "SET_PROMPT_MODE", mode: promptMode } }, "*");
  });
}

/* Persist the activity log setting through the main thread. */
if (showLogEl) {
  showLogEl.addEventListener("change", () => {
    showLog = showLogEl.checked;
    showLogSetting();
    parent.postMessage({ pluginMessage: { type: "SET_SHOW_LOG", show: showLog } }, "*");
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

function isChallengeFrame(frame: unknown): boolean {
  return (
    typeof frame === "object" &&
    frame !== null &&
    (frame as { type?: unknown }).type === "CHALLENGE"
  );
}

function isHeartbeatFrame(frame: unknown): boolean {
  return (
    typeof frame === "object" &&
    frame !== null &&
    (frame as { type?: unknown }).type === "HEARTBEAT"
  );
}

/** Drops the current socket and runs its close bookkeeping, with no scheduled retry. */
let detachCurrent: (() => void) | null = null;

/** True once the current socket passed the daemon challenge. */
let verified = false;

/** Opens a WebSocket and registers lifecycle handlers. */
function connect(): void {
  /* Cancel any pending reconnect so a stale timer cannot open a second socket. */
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  /* A build with no real token (the public placeholder) must never open a socket. */
  if (!hasPairingToken(__TURBOFIG_PAIRING_TOKEN__)) {
    if (connStatusEl) {
      connStatusEl.textContent = NO_TOKEN_LABEL;
      connStatusEl.className = "error";
    }
    return;
  }
  setConnStatus("attempt");
  const nonce = generateNonce((bytes) => crypto.getRandomValues(bytes));
  /* The daemon's proof names this port: verify against the port connected to. */
  const port = currentPort;
  const socket = new WebSocket(buildSocketUrl(port, nonce));
  ws = socket;
  liveness.newSocket();
  /* The socket carries no job until the daemon proves it knows the token. */
  verified = false;
  let failed = false;
  let sawChallenge = false;

  /* Close the socket, run nothing, and show the failure after the normal backoff. */
  const failVerification = (): void => {
    failed = true;
    socket.onmessage = null;
    socket.close();
  };

  socket.onopen = () => {
    /* Wait for the CHALLENGE. Status and FILE_INFO follow verification. */
  };

  /* Close bookkeeping shared by onclose and an immediate reconnect. It never schedules a retry. */
  const finishClose = (): void => {
    if (ws === socket) verified = false;
    /* No result can arrive on a closed socket: end the working state. */
    activeJobs = new Set();
    setWorking();
    linkHealth = healthAfterClose(linkHealth, { sawChallenge, failedVerification: failed });
  };
  socket.onclose = () => {
    finishClose();
    scheduleReconnect();
  };
  /* Drop this socket for an immediate reconnect: no second reconnect is scheduled. */
  detachCurrent = () => {
    socket.onclose = null;
    socket.onmessage = null;
    socket.close();
    finishClose();
  };

  socket.onerror = () => {
    /* onclose fires after onerror; let onclose drive the reconnect. */
  };

  /* Forward daemon request messages to the main thread. */
  socket.onmessage = (event: MessageEvent) => {
    liveness.touch();
    let candidate: unknown;
    try {
      candidate = JSON.parse(event.data as string);
      if (verified && isHeartbeatFrame(candidate)) liveness.onHeartbeat();
    } catch {
      if (!verified) failVerification();
      return;
    }
    if (isChallengeFrame(candidate)) sawChallenge = true;
    const decision = decideFrame(verified, candidate, nonce, __TURBOFIG_PAIRING_TOKEN__, port);
    if (decision.action === "close") {
      failVerification();
      return;
    }
    if (decision.action === "ignore") return;
    if (decision.action === "auth") {
      socket.send(JSON.stringify(decision.auth));
      verified = true;
      attempt = 0;
      linkHealth = INITIAL_LINK_HEALTH;
      setConnStatus("open");
      /* Re-identify the file to the daemon on every successful connect. */
      if (latestFileInfo) {
        socket.send(JSON.stringify(latestFileInfo));
      }
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
      daemonBinPath = typeof parsed.binPath === "string" ? parsed.binPath : "";
      daemonBridgeDirCustom =
        typeof parsed.bridgeDirCustom === "string" ? parsed.bridgeDirCustom : "";
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

/* Liveness: reconnect at once when the daemon goes silent or the window wakes with a stale link. */
const liveness = createLiveness({
  now: () => Date.now(),
  readyState: () => (ws ? ws.readyState : null),
  activeJobs: () => activeJobs.size,
  backoffPending: () => reconnectTimer !== null,
  reconnectNow: () => {
    if (!hasConnectedOnce) return;
    if (ws) detachCurrent?.();
    attempt = 0;
    connect();
  },
  setTimer: (fn, ms) => setInterval(fn, ms),
  clearTimer: (h) => clearInterval(h as ReturnType<typeof setInterval>),
});
liveness.start();
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") liveness.onWake();
});
window.addEventListener("online", () => liveness.onWake());
window.addEventListener("focus", () => liveness.onWake());

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
  if (action === "promptmode") {
    promptMode = parsePromptMode(m.mode);
    showPromptMode();
    return;
  }
  if (action === "showlog") {
    showLog = parseShowLog(m.show);
    showLogSetting();
    return;
  }
  if (action === "fileinfo") {
    latestFileInfo = m as unknown as FileInfoMessage;
    updateFileDisplay();
    /* If already connected, send FILE_INFO now so it reaches the daemon. */
    if (ws && verified && ws.readyState === 1 /* OPEN */) {
      ws.send(JSON.stringify(m));
    }
    return;
  }
  /* Count running jobs from STARTED and RESULT for the working indicator. */
  const nextJobs = trackActiveJobs(activeJobs, m.type, m.requestId);
  if (nextJobs !== activeJobs) {
    activeJobs = nextJobs;
    setWorking();
  }
  /* Relay RESULT and all other main-thread messages over the WS. */
  if (ws && verified && ws.readyState === 1 /* OPEN */) {
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
