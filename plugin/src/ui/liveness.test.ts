import { describe, expect, test } from "bun:test";
import {
  createLiveness,
  isSilent,
  type LivenessHost,
  SILENCE_LIMIT_MS,
  shouldReconnectOnSilence,
  shouldReconnectOnWake,
  WATCHDOG_INTERVAL_MS,
} from "./liveness";

/** Fake host: virtual clock, one repeating timer, a settable socket state. */
function fakeHost(initialState: number | null = 1) {
  let t = 1_000_000;
  let tick: (() => void) | null = null;
  const h = {
    reconnects: 0,
    jobs: 0,
    backoff: false,
    state: initialState as number | null,
    advance(ms: number) {
      // Run the watchdog at each interval boundary, like a real timer.
      let left = ms;
      while (left > 0) {
        const step = Math.min(left, WATCHDOG_INTERVAL_MS);
        t += step;
        left -= step;
        if (step === WATCHDOG_INTERVAL_MS) tick?.();
      }
    },
    host: {
      now: () => t,
      readyState: () => h.state,
      activeJobs: () => h.jobs,
      backoffPending: () => h.backoff,
      reconnectNow: () => {
        h.reconnects += 1;
      },
      setTimer: (fn: () => void) => {
        tick = fn;
        return 1;
      },
      clearTimer: () => {
        tick = null;
      },
    } satisfies LivenessHost,
  };
  return h;
}

describe("liveness pure checks", () => {
  test("isSilent is true only past the limit", () => {
    expect(isSilent(40_000, 0)).toBe(false);
    expect(isSilent(40_001, 0)).toBe(true);
  });
  const wake = (o: Partial<Parameters<typeof shouldReconnectOnWake>[0]>) =>
    shouldReconnectOnWake({
      readyState: 1,
      now: 100,
      lastFrameAt: 100,
      activeJobs: 0,
      backoffPending: false,
      ...o,
    });
  test("wake: CONNECTING never reconnects", () => {
    expect(wake({ readyState: 0, now: 999_999 })).toBe(false);
  });
  test("wake: CLOSING or CLOSED reconnects unless a backoff timer is pending", () => {
    expect(wake({ readyState: 2 })).toBe(true);
    expect(wake({ readyState: 3 })).toBe(true);
    expect(wake({ readyState: 3, backoffPending: true })).toBe(false);
  });
  test("wake: OPEN reconnects only when the frame is older than 20 s", () => {
    expect(wake({ now: 20_000, lastFrameAt: 0 })).toBe(false);
    expect(wake({ now: 20_001, lastFrameAt: 0 })).toBe(true);
  });
  test("wake: OPEN with an active job never reconnects", () => {
    expect(wake({ now: 99_999, lastFrameAt: 0, activeJobs: 1 })).toBe(false);
  });
  test("silence: needs the armed flag", () => {
    const base = { readyState: 1, now: 100_000, lastFrameAt: 0, armed: true, activeJobs: 0 };
    expect(shouldReconnectOnSilence(base)).toBe(true);
    expect(shouldReconnectOnSilence({ ...base, armed: false })).toBe(false);
  });
  test("silence: an active job on an OPEN socket skips the watchdog", () => {
    const base = { readyState: 1, now: 100_000, lastFrameAt: 0, armed: true, activeJobs: 1 };
    expect(shouldReconnectOnSilence(base)).toBe(false);
  });
});

describe("createLiveness", () => {
  test("reconnects after 40 s of silence, not before", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    l.start();
    l.onHeartbeat();
    h.advance(SILENCE_LIMIT_MS);
    expect(h.reconnects).toBe(0);
    h.advance(WATCHDOG_INTERVAL_MS);
    expect(h.reconnects).toBe(1);
  });

  test("frames keep the link alive", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    l.start();
    for (let i = 0; i < 20; i++) {
      h.advance(15_000);
      l.touch();
    }
    expect(h.reconnects).toBe(0);
  });

  test("one stale link gives one reconnect, not a storm", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    l.start();
    l.onHeartbeat();
    h.advance(60_000);
    expect(h.reconnects).toBe(1);
  });

  test("a stale link reconnects at once on a wake event", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    h.advance(21_000);
    l.onWake();
    expect(h.reconnects).toBe(1);
  });

  test("a fresh link ignores a wake event", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    h.advance(5_000);
    l.onWake();
    expect(h.reconnects).toBe(0);
  });

  test("a CLOSED socket reconnects on wake when no backoff is pending", () => {
    const h = fakeHost(3);
    const l = createLiveness(h.host);
    l.onWake();
    expect(h.reconnects).toBe(1);
  });

  test("a CLOSED socket with a pending backoff is left alone on wake", () => {
    const h = fakeHost(3);
    h.backoff = true;
    const l = createLiveness(h.host);
    l.onWake();
    expect(h.reconnects).toBe(0);
  });

  test("a CONNECTING socket is left alone on wake", () => {
    const h = fakeHost(0);
    const l = createLiveness(h.host);
    h.advance(60_000);
    l.onWake();
    expect(h.reconnects).toBe(0);
  });

  test("3 simultaneous wake events give at most 1 reconnect", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    h.advance(30_000);
    // Like the real host: the reconnect opens a CONNECTING socket.
    h.host.reconnectNow = () => {
      h.reconnects += 1;
      h.state = 0;
    };
    l.onWake();
    l.onWake();
    l.onWake();
    expect(h.reconnects).toBe(1);
  });

  test("an older daemon with no HEARTBEAT is never dropped for silence", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    l.start();
    h.advance(300_000);
    expect(h.reconnects).toBe(0);
  });

  test("a new socket disarms the watchdog until its first HEARTBEAT", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    l.start();
    l.onHeartbeat();
    l.newSocket();
    h.advance(300_000);
    expect(h.reconnects).toBe(0);
    l.touch();
    l.onHeartbeat();
    h.advance(SILENCE_LIMIT_MS + WATCHDOG_INTERVAL_MS);
    expect(h.reconnects).toBe(1);
  });

  test("an active job on an OPEN socket skips the watchdog and the stale wake", () => {
    const h = fakeHost();
    h.jobs = 1;
    const l = createLiveness(h.host);
    l.start();
    l.onHeartbeat();
    h.advance(120_000);
    l.onWake();
    expect(h.reconnects).toBe(0);
  });

  test("no socket at all means no action", () => {
    const h = fakeHost(null);
    const l = createLiveness(h.host);
    l.start();
    h.advance(120_000);
    l.onWake();
    expect(h.reconnects).toBe(0);
  });

  test("a closed socket is left to the backoff timer", () => {
    const h = fakeHost(3);
    const l = createLiveness(h.host);
    l.start();
    h.advance(120_000);
    expect(h.reconnects).toBe(0);
  });

  test("stop ends the watchdog", () => {
    const h = fakeHost();
    const l = createLiveness(h.host);
    l.start();
    l.stop();
    h.advance(120_000);
    expect(h.reconnects).toBe(0);
  });
});
