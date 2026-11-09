/**
 * Link liveness for the plugin UI socket. Pure logic with injected clock,
 * timer and socket state, so it runs in tests with no DOM.
 *
 * A browser answers protocol pings by itself, so plugin JavaScript cannot see
 * a dead socket. The daemon sends a HEARTBEAT text frame every 15 s instead.
 * The UI records the time of the last daemon frame and reconnects when the
 * link goes quiet or the window wakes up with a stale link. The watchdog arms
 * after the first HEARTBEAT on a socket, so an older daemon is not dropped.
 */

/** WebSocket readyState value for an open socket. */
export const READY_STATE_OPEN = 1;

/** WebSocket readyState value for a closing socket. CLOSING and CLOSED need no watchdog. */
export const READY_STATE_CLOSING = 2;

/** Close the socket and reconnect when no daemon frame arrives for this long. */
export const SILENCE_LIMIT_MS = 40_000;

/** On wake, reconnect when the last daemon frame is older than this. */
export const WAKE_STALE_MS = 20_000;

/** How often the silence watchdog checks the clock. */
export const WATCHDOG_INTERVAL_MS = 5_000;

/** True when the link has been silent longer than `limitMs`. */
export function isSilent(now: number, lastFrameAt: number, limitMs = SILENCE_LIMIT_MS): boolean {
  return now - lastFrameAt > limitMs;
}

/** Inputs for a wake decision. */
export interface WakeInput {
  readyState: number;
  now: number;
  lastFrameAt: number;
  /** Count of jobs that wait for a result. */
  activeJobs: number;
  /** True when a backoff timer already waits to reconnect. */
  backoffPending: boolean;
  staleMs?: number;
}

/**
 * True when a wake event (visible, online, focus) must reconnect at once.
 * - CONNECTING: never. The handshake is already running.
 * - CLOSING or CLOSED: yes, unless a backoff timer already owns the retry.
 * - OPEN: only when the last daemon frame is stale and no job is active.
 *   A long job can freeze the UI, so a stale clock is not proof of a dead link.
 */
export function shouldReconnectOnWake(input: WakeInput): boolean {
  const { readyState, now, lastFrameAt, activeJobs, backoffPending } = input;
  if (readyState >= READY_STATE_CLOSING) return !backoffPending;
  if (readyState !== READY_STATE_OPEN) return false;
  if (activeJobs > 0) return false;
  return now - lastFrameAt > (input.staleMs ?? WAKE_STALE_MS);
}

/** Inputs for a silence watchdog decision. */
export interface SilenceInput {
  readyState: number;
  now: number;
  lastFrameAt: number;
  /** True after the first HEARTBEAT on the current socket. */
  armed: boolean;
  activeJobs: number;
}

/**
 * True when the watchdog must reconnect. It stays off until the first
 * HEARTBEAT on this socket (an older daemon sends none) and while a job runs
 * on an OPEN socket (the daemon's pong drop covers a dead link).
 */
export function shouldReconnectOnSilence(input: SilenceInput): boolean {
  const { readyState, now, lastFrameAt, armed, activeJobs } = input;
  if (!armed) return false;
  if (readyState >= READY_STATE_CLOSING) return false;
  if (readyState === READY_STATE_OPEN && activeJobs > 0) return false;
  return isSilent(now, lastFrameAt);
}

/** What the controller needs from its host. */
export interface LivenessHost {
  /** Current time in ms. */
  now(): number;
  /** Current socket readyState, or null when no socket exists. */
  readyState(): number | null;
  /** Count of jobs that wait for a result. */
  activeJobs(): number;
  /** True when a backoff timer already waits to reconnect. */
  backoffPending(): boolean;
  /** Close the current socket and open a new one at once. */
  reconnectNow(): void;
  /** Start a repeating timer. Returns a handle for `clearTimer`. */
  setTimer(fn: () => void, ms: number): unknown;
  clearTimer(handle: unknown): void;
}

/** Liveness controller. */
export interface Liveness {
  /** Record that a daemon frame arrived. */
  touch(): void;
  /** Record that a HEARTBEAT arrived. Arms the silence watchdog for this socket. */
  onHeartbeat(): void;
  /** Record that a new socket opened. Disarms the watchdog until its first HEARTBEAT. */
  newSocket(): void;
  /** Handle a wake event (visibility to visible, online, focus). */
  onWake(): void;
  /** Start the silence watchdog. */
  start(): void;
  /** Stop the silence watchdog. */
  stop(): void;
}

/** Builds a liveness controller around the host. */
export function createLiveness(host: LivenessHost): Liveness {
  let lastFrameAt = host.now();
  let timer: unknown = null;
  let armed = false;

  const reconnect = (): void => {
    // Count the new socket as fresh, so one stale link gives one reconnect.
    lastFrameAt = host.now();
    host.reconnectNow();
  };

  return {
    touch(): void {
      lastFrameAt = host.now();
    },
    onHeartbeat(): void {
      armed = true;
    },
    newSocket(): void {
      armed = false;
      lastFrameAt = host.now();
    },
    onWake(): void {
      const state = host.readyState();
      if (state === null) return;
      const input = {
        readyState: state,
        now: host.now(),
        lastFrameAt,
        activeJobs: host.activeJobs(),
        backoffPending: host.backoffPending(),
      };
      if (shouldReconnectOnWake(input)) reconnect();
    },
    start(): void {
      if (timer !== null) return;
      timer = host.setTimer(() => {
        const state = host.readyState();
        // No socket, or a closed one: the backoff timer already owns the retry.
        if (state === null) return;
        const input = {
          readyState: state,
          now: host.now(),
          lastFrameAt,
          armed,
          activeJobs: host.activeJobs(),
        };
        if (shouldReconnectOnSilence(input)) reconnect();
      }, WATCHDOG_INTERVAL_MS);
    },
    stop(): void {
      if (timer === null) return;
      host.clearTimer(timer);
      timer = null;
    },
  };
}
