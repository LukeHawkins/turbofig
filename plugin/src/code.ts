import { createTf } from "./helpers";
import type {
  ExecuteMessage,
  GetSelectionMessage,
  InboundMessage,
  ResultMessage,
  ScreenshotMessage,
} from "./protocol";
import {
  buildExecuteError,
  buildExecuteSuccess,
  buildFileInfo,
  buildResult,
  buildScreenshot,
  buildSelection,
  buildStarted,
  CLOSE_FOR_UPDATE_TOAST,
  capResultMessage,
  isAllowedExternalUrl,
  isInboundMessage,
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
 * Returns a plain string message from a thrown value.
 * Handles an Error (reads .message), a thrown primitive (e.g. throw "boom"),
 * and anything else via String(). Never throws itself.
 */
function errorMessageOf(err: unknown): string {
  try {
    return err != null && typeof err === "object" && "message" in err
      ? String((err as { message: unknown }).message)
      : String(err);
  } catch {
    // A hostile or odd thrown value (null prototype, throwing getter): never throw here.
    return "non-printable thrown value";
  }
}

/**
 * Returns a one-line hint for a common execute error, or null.
 * The sync-to-async table already reaches the consumer in the eval preamble,
 * so only the generic dynamic-page message gets a short pointer here.
 */
function errorHint(msg: string, tf: object): string | null {
  if (/unloaded font|font .*not loaded/i.test(msg)) {
    return "call await figma.loadFontAsync(node.fontName) first; tf.text loads fonts itself";
  }
  if (/dynamic-page|Async variant|getNodeById\b(?!Async)|documentAccess/i.test(msg)) {
    return "use the Async variant, e.g. await figma.getNodeByIdAsync(id)";
  }
  if (/\btf\.\w+ is not a function/.test(msg)) {
    let list = "";
    try {
      const names = Object.keys(tf)
        .filter((k) => typeof (tf as Record<string, unknown>)[k] === "function")
        .sort();
      list = `. tf has: ${names.join(", ")}`;
    } catch {
      // User code can change tf (e.g. a throwing getter): fall back to no list.
    }
    return `that tf helper does not exist${list}`;
  }
  if (/cannot read propert(y|ies).* of null|null is not an object/i.test(msg)) {
    return "a lookup returned null: the node was not found; check the id";
  }
  return null;
}

/** Splits a stack line into the text before ":line:col" and the numbers, or null. */
function parseFrame(frame: string): { prefix: string; line: number; column: number } | null {
  const m = frame.match(/^(.*):(\d+):(\d+)\)?\s*$/);
  // V8 puts the host call site in the prefix; drop it so probe and run match.
  return m
    ? { prefix: m[1].replace(/eval at .*?\), /, ""), line: Number(m[2]), column: Number(m[3]) }
    : null;
}

/** Stack lines of an error, or an empty list. */
function stackLines(err: unknown): string[] {
  const stack =
    err != null && typeof err === "object" && "stack" in err
      ? String((err as { stack: unknown }).stack)
      : "";
  return stack.split("\n");
}

/** The AsyncFunction constructor of the current engine. */
function asyncFunctionCtor(): new (...args: string[]) => (...args: unknown[]) => Promise<unknown> {
  return Object.getPrototypeOf(async () => {}).constructor;
}

let calibration: Promise<{ prefix: string; offset: number } | null> | undefined;

/**
 * Learns once, at runtime, how this engine reports positions in eval'd code.
 * It runs a probe that throws on user line 2 through the same wrapUserCode
 * path. The first stack frame gives the frame prefix (file or eval marker)
 * and the raw line L0, so offset = L0 - 2. Returns null when the engine
 * gives no usable frame.
 */
function calibrate(): Promise<{ prefix: string; offset: number } | null> {
  calibration ??= (async () => {
    try {
      const fn = new (asyncFunctionCtor())("figma", "tf", wrapUserCode("\nthrow new Error()"));
      await fn({}, {});
    } catch (err) {
      for (const l of stackLines(err)) {
        const f = parseFrame(l);
        if (f) return { prefix: f.prefix, offset: f.line - 2 };
      }
    }
    return null;
  })();
  return calibration;
}

/** Pure mapper: first frame with the calibrated prefix, in range of the user code, else null. */
export function mapStackToUserLine(
  stack: string[],
  cal: { prefix: string; offset: number },
  lineCount: number,
): { line: number; column: number } | null {
  for (const l of stack) {
    const f = parseFrame(l);
    if (!f || f.prefix !== cal.prefix) continue;
    const line = f.line - cal.offset;
    return line >= 1 && line <= lineCount ? { line, column: f.column } : null;
  }
  return null;
}

/**
 * Maps an error to a position in the user's own code. Only stack frames with
 * the calibrated prefix count (see mapStackToUserLine). Returns null when
 * calibration fails: a wrong line is worse than none.
 */
async function userCodePosition(
  err: unknown,
  lineCount: number,
): Promise<{ line: number; column: number } | null> {
  const cal = await calibrate();
  return cal ? mapStackToUserLine(stackLines(err), cal, lineCount) : null;
}

/**
 * Default milliseconds budget (queue wait + run time) for a queued job whose
 * daemon-supplied timeoutMs is missing or non-positive.
 */
const DEFAULT_QUEUE_TIMEOUT_MS = 30_000;

/**
 * Largest delay setTimeout accepts before it overflows a 32-bit signed int
 * and fires almost immediately instead of waiting. A daemon-supplied
 * timeoutMs is clamped to this independently of the daemon's own clamp
 * (TURBOFIG_REQUEST_TIMEOUT_MS), since the plugin cannot trust the daemon
 * never to send an oversized value.
 */
const MAX_SETTIMEOUT_MS = 2 ** 31 - 1;

/** Clamps a milliseconds delay to the largest value setTimeout accepts. */
function clampTimeoutMs(ms: number): number {
  return Math.min(ms, MAX_SETTIMEOUT_MS);
}

/**
 * Validates and persists a daemon WebSocket port.
 * Returns the saved port when valid (integer, 1-65535), or null when invalid.
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
 *
 * When msg.timeoutMs is set, the plugin stops waiting at that point and replies
 * ok:false with a timeout error. A synchronous infinite loop in the user code
 * cannot be interrupted this way (JavaScript is single-threaded, so the timer
 * never fires until the loop yields); that is a documented limit, not a bug.
 */
export async function handleExecute(
  figma: PluginAPI,
  tf: ReturnType<typeof createTf>,
  msg: ExecuteMessage,
): Promise<ResultMessage> {
  const { requestId, code, timeoutMs } = msg;
  const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (
    ...args: string[]
  ) => (...args: unknown[]) => Promise<unknown>;

  const run = async (): Promise<ResultMessage> => {
    try {
      // Build and run the user code in an async function with figma and tf in scope.
      let fn: (...args: unknown[]) => Promise<unknown>;
      try {
        fn = new AsyncFunction("figma", "tf", wrapUserCode(code));
      } catch (buildErr) {
        // A SyntaxError here has no position in the user code: add a hint, not a line.
        if (buildErr != null && (buildErr as { name?: unknown }).name === "SyntaxError") {
          return buildExecuteError(
            requestId,
            `${errorMessageOf(buildErr)}\nHint: syntax error in your code: check quotes, brackets and commas; do not redeclare figma or tf`,
          );
        }
        throw buildErr;
      }
      const result = await fn(figma, tf);
      return buildExecuteSuccess(requestId, safeResult(result));
    } catch (err) {
      try {
        const pos = await userCodePosition(err, code.split("\n").length);
        const base = errorMessageOf(err);
        let message = pos ? `${base} (line ${pos.line}, column ${pos.column})` : base;
        if (pos) {
          const srcLine = code.split("\n")[pos.line - 1]?.trim();
          if (srcLine) {
            message += `\n> ${srcLine.length > 120 ? `${srcLine.slice(0, 120)}...` : srcLine}`;
          }
        }
        const hint = errorHint(base, tf);
        if (hint) message += `\nHint: ${hint}`;
        return buildExecuteError(requestId, message);
      } catch {
        return buildExecuteError(requestId, "non-printable thrown value");
      }
    }
  };

  if (typeof timeoutMs !== "number" || timeoutMs <= 0) return run();

  const timeout = new Promise<ResultMessage>((resolve) => {
    setTimeout(() => {
      resolve(
        buildExecuteError(
          requestId,
          `job timed out in the plugin after ${timeoutMs}ms; it may still be running; a retry is not idempotent`,
          "started_unknown",
        ),
      );
    }, clampTimeoutMs(timeoutMs));
  });
  return Promise.race([run(), timeout]);
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
    return buildExecuteError(requestId, errorMessageOf(err));
  }
}

/** A rectangle with a size, as in absoluteRenderBounds. */
interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

function boundsOf(n: unknown, renderFirst = true): Bounds | null {
  const o = n as { absoluteRenderBounds?: Bounds | null; absoluteBoundingBox?: Bounds | null };
  const b = renderFirst ? (o.absoluteRenderBounds ?? o.absoluteBoundingBox) : o.absoluteBoundingBox;
  return b && Number.isFinite(b.width) && Number.isFinite(b.height) ? b : null;
}

/**
 * Longest edge, in px, of the PNG a node exports at scale 1, or 0 when unknown.
 * Prefers absoluteRenderBounds (it includes effects). A page uses the union of
 * its top-level children. Falls back to width and height.
 */
function longestEdge(node: BaseNode): number {
  const n = node as unknown as {
    type?: string;
    children?: unknown[];
    width?: number;
    height?: number;
  };
  if (n.type === "PAGE") {
    let minX = Number.POSITIVE_INFINITY;
    let minY = Number.POSITIVE_INFINITY;
    let maxX = Number.NEGATIVE_INFINITY;
    let maxY = Number.NEGATIVE_INFINITY;
    for (const c of n.children ?? []) {
      const b = boundsOf(c, false);
      if (!b) continue;
      minX = Math.min(minX, b.x);
      minY = Math.min(minY, b.y);
      maxX = Math.max(maxX, b.x + b.width);
      maxY = Math.max(maxY, b.y + b.height);
    }
    return Number.isFinite(minX) ? Math.max(maxX - minX, maxY - minY) : 0;
  }
  const b = boundsOf(node);
  const edge = b ? Math.max(b.width, b.height) : Math.max(n.width ?? 0, n.height ?? 0);
  return Number.isFinite(edge) ? edge : 0;
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
    // Clamp scale to the documented range: Figma rejects an out-of-range
    // constraint, and an unbounded scale can produce an export far past the
    // result size cap.
    const requested = typeof msg.scale === "number" && msg.scale > 0 ? msg.scale : 1;
    let scale = Math.min(4, Math.max(0.1, requested));
    // Optional daemon cap on the longest edge. The typings document no SCALE
    // limit below 4, so the floor stays 0.01. Unknown bounds skip the cap.
    if (typeof msg.maxDim === "number" && msg.maxDim > 0) {
      let longest = 0;
      try {
        longest = longestEdge(node);
      } catch {
        // Unreadable bounds (e.g. an unloaded page): skip the cap.
      }
      if (longest > 0) {
        // Floor to 4 dp so float error never exports maxDim + 1 px, which
        // would make the daemon decode and resize the whole PNG for 1 px.
        const cap = Math.floor((msg.maxDim / longest) * 1e4) / 1e4;
        scale = Math.min(4, Math.max(0.01, Math.min(scale, cap)));
      }
    }
    const bytes = await exportable.exportAsync({
      format: "PNG",
      constraint: { type: "SCALE", value: scale },
    });
    const png = figma.base64Encode(bytes);
    return buildScreenshot(requestId, png, exportable.width, exportable.height);
  } catch (err) {
    return buildExecuteError(requestId, errorMessageOf(err));
  }
}

/** Build-time constant injected by build-code.ts via Bun.build define. */
declare const __PLUGIN_VERSION__: string;

/**
 * Returns the build-time plugin version, or "dev" when running outside a
 * `bun run build` bundle (e.g. under `bun test`, which never defines
 * `__PLUGIN_VERSION__`). `typeof` never throws on an undeclared identifier,
 * unlike a direct reference, so this is safe in both contexts.
 */
function pluginVersion(): string {
  return typeof __PLUGIN_VERSION__ !== "undefined" ? __PLUGIN_VERSION__ : "dev";
}

/** Posts a FILE_INFO message with the current file identity via the given sink. */
function emitFileInfo(figma: PluginAPI, post: (msg: unknown) => void): void {
  post(buildFileInfo(figma.fileKey ?? "", figma.root.name, pluginVersion()));
}

/** Reads the stored daemon port (falling back to the default) and posts it via the given sink. */
async function emitStoredPort(figma: PluginAPI, post: (msg: unknown) => void): Promise<void> {
  let stored: unknown;
  try {
    stored = await figma.clientStorage.getAsync("turbofig:wsPort");
  } catch {
    // clientStorage is unavailable or the read failed; fall back to the default port.
    stored = undefined;
  }
  const port =
    typeof stored === "number" && Number.isInteger(stored) && stored >= 1 && stored <= 65535
      ? stored
      : 18847;
  post({ type: "PORT", port });
}

/** clientStorage key for the "Activity log" on/off setting. */
const SHOW_LOG_KEY = "turbofig:showLog";

/** Reads the stored log setting (true by default) and posts it via the given sink. */
async function emitStoredShowLog(figma: PluginAPI, post: (msg: unknown) => void): Promise<void> {
  let stored: unknown;
  try {
    stored = await figma.clientStorage.getAsync(SHOW_LOG_KEY);
  } catch {
    stored = undefined;
  }
  post({ type: "SHOW_LOG", show: stored !== false });
}

/**
 * Builds a ResultMessage for a job that expired while still waiting in the
 * queue: it never ran, so a retry is unconditionally safe.
 */
function queueExpiredResult(requestId: number): ResultMessage {
  return buildExecuteError(
    requestId,
    "expired in the queue; the job did not run; a retry is safe",
    "not_started",
  );
}

/**
 * Builds a ResultMessage for a job whose combined queue-wait-plus-run budget
 * elapsed while it was actually running: unlike queueExpiredResult, the job
 * may still be executing, so a retry is not idempotent.
 */
function queueRunTimeoutResult(requestId: number, ms: number): ResultMessage {
  return buildExecuteError(
    requestId,
    `job timed out after ${ms}ms (queue wait + run time); it may still be running; a retry is not idempotent`,
    "started_unknown",
  );
}

/** Resolves with queueRunTimeoutResult after `ms`, to race against a running job. */
function queueRunTimeout(requestId: number, ms: number): Promise<ResultMessage> {
  return new Promise((resolve) => {
    setTimeout(() => resolve(queueRunTimeoutResult(requestId, ms)), clampTimeoutMs(ms));
  });
}

/**
 * Builds the onmessage dispatcher for the plugin main thread.
 * Exported for testing without a live Figma environment: pass a mock
 * PluginAPI and a `post` sink in place of figma.ui.postMessage.
 *
 * EXECUTE, GET_SELECTION and SCREENSHOT run one at a time, FIFO, through a
 * single queue: the protocol contract that stops interleaved jobs from
 * creating duplicate nodes. STATUS, SET_PORT, RESIZE and READY bypass the
 * queue and run immediately. A queued reply is capped at 16 MiB
 * (capResultMessage) before it reaches `post`.
 *
 * Each queued job carries a deadline stamped at enqueue time (when the
 * daemon's message was received), not at the moment it reaches the front of
 * the queue: a job that already queued past its own timeoutMs never runs at
 * all, and replies ok:false immediately instead of running late. A job that
 * is still within its deadline when it starts races against the remaining
 * time, so GET_SELECTION and SCREENSHOT (not only EXECUTE) can never hang
 * the queue forever on a stuck exportAsync/getNodeByIdAsync.
 */
export function createDispatcher(
  figma: PluginAPI,
  post: (msg: unknown) => void,
): (raw: unknown) => void {
  // Chained promise: each queued job runs only after the previous one settles.
  let queueTail: Promise<void> = Promise.resolve();
  // Jobs queued or running. The Reload close request waits until this is 0.
  let pendingJobs = 0;

  async function runQueuedJob(
    requestId: number,
    deadline: number,
    job: () => Promise<ResultMessage>,
  ): Promise<ResultMessage> {
    const remaining = deadline - Date.now();
    if (remaining <= 0) return queueExpiredResult(requestId);
    // Only announce STARTED once the job is actually about to run: a job
    // that expired in the queue above never ran, so it must never be told
    // apart as "started" by a daemon-side caller checking for idempotency.
    // A failing post (e.g. a transiently closed UI channel) must never stop
    // the job itself from running: STARTED is a best-effort progress signal,
    // not a precondition for the actual work.
    try {
      post(buildStarted(requestId));
    } catch {
      // Swallowed: see comment above.
    }
    return Promise.race([job(), queueRunTimeout(requestId, remaining)]);
  }

  function enqueue(
    requestId: number,
    timeoutMs: number | undefined,
    job: () => Promise<ResultMessage>,
  ): void {
    const budget =
      typeof timeoutMs === "number" && timeoutMs > 0 ? timeoutMs : DEFAULT_QUEUE_TIMEOUT_MS;
    const deadline = Date.now() + clampTimeoutMs(budget);
    pendingJobs += 1;
    queueTail = queueTail
      .then(() => runQueuedJob(requestId, deadline, job))
      .then((reply) => post(capResultMessage(reply)))
      .catch(() => {
        // A prior link's job or post threw: swallow it here so queueTail
        // always settles back to resolved. Without this, one throw leaves
        // queueTail permanently rejected and every later .then() in the
        // chain (i.e. every later queued job) is skipped and silently
        // dropped instead of running.
      })
      .then(() => {
        pendingJobs -= 1;
      });
  }

  return (raw: unknown): void => {
    if (!isInboundMessage(raw)) return;
    const msg: InboundMessage = raw;
    switch (msg.type) {
      case "READY":
        /* The UI sends this once, on load. Reply with the file identity and
           the stored port so neither message can be missed by a late listener. */
        emitFileInfo(figma, post);
        void emitStoredPort(figma, post);
        void emitStoredShowLog(figma, post);
        break;
      case "STATUS":
        post(buildResult(msg, figma.fileKey ?? "", figma.root.name));
        break;
      case "EXECUTE": {
        // Build the tf namespace bound to this live PluginAPI instance.
        const tf = createTf(figma);
        enqueue(msg.requestId, msg.timeoutMs, () => handleExecute(figma, tf, msg));
        break;
      }
      case "GET_SELECTION":
        enqueue(msg.requestId, msg.timeoutMs, () => handleGetSelection(figma, msg));
        break;
      case "SCREENSHOT":
        enqueue(msg.requestId, msg.timeoutMs, () => handleScreenshot(figma, msg));
        break;
      case "SET_PORT":
        /* Validate and persist the port; send PORT back so the UI can reconnect. */
        applySetPort(figma.clientStorage, msg.port)
          .then((saved) => {
            if (saved !== null) post({ type: "PORT", port: saved });
          })
          .catch(() => {
            // Persisting the port failed (e.g. clientStorage quota/unavailable);
            // the UI keeps its current port and the user can retry.
          });
        break;
      case "SET_SHOW_LOG":
        /* Persist the setting; the UI already shows it, so no reply is needed. */
        figma.clientStorage.setAsync(SHOW_LOG_KEY, msg.show).catch(() => {
          // Persisting failed; the choice still holds for this session.
        });
        break;
      case "OPEN_URL":
        /* Open an external link only when it is on the allowlist. */
        if (isAllowedExternalUrl(msg.url)) figma.openExternal(msg.url);
        break;
      case "RESIZE":
        /* Resize the plugin panel to the dimensions requested by the UI. */
        if (msg.width > 0 && msg.height > 0) {
          figma.ui.resize(msg.width, msg.height);
        }
        break;
      case "CLOSE_FOR_UPDATE":
        /* The UI's Reload button. Refuse while a job is queued or running. */
        if (pendingJobs === 0) figma.closePlugin(CLOSE_FOR_UPDATE_TOAST);
        break;
      default:
        break;
    }
  };
}

/** How often to poll for a file rename, in milliseconds. A plain property read, not an API call. */
const FILE_NAME_POLL_MS = 5000;

/**
 * Polls figma.root.name and re-emits FILE_INFO when it changes (a file rename).
 * The Plugin API has no change event for the document/file name, only for
 * node properties, so a cheap interval poll is the only practical option.
 */
function watchFileName(figma: PluginAPI, post: (msg: unknown) => void): void {
  let lastName = figma.root.name;
  setInterval(() => {
    if (figma.root.name !== lastName) {
      lastName = figma.root.name;
      emitFileInfo(figma, post);
    }
  }, FILE_NAME_POLL_MS);
}

// Plugin bootstrap. Guard with a globalThis check so this file is importable in unit tests.
// In the Figma sandbox, globalThis.figma is the PluginAPI. Outside it, the block is skipped.
if ((globalThis as Record<string, unknown>).figma !== undefined) {
  // themeColors makes Figma inject the --figma-color-* variables and a
  // figma-light/figma-dark class, so the panel matches the user's theme.
  figma.showUI(__html__, { width: 300, height: 150, themeColors: true });

  /* The UI sends READY once it has loaded; the dispatcher replies with
     FILE_INFO and PORT. This handshake means the UI can never miss either
     message, and the main thread can re-announce FILE_INFO later
     (see watchFileName) on the same channel. */
  figma.ui.onmessage = createDispatcher(figma, (msg) => figma.ui.postMessage(msg));

  watchFileName(figma, (msg) => figma.ui.postMessage(msg));
}
