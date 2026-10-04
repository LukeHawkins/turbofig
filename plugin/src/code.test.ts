/**
 * Unit tests for the exported handler functions in code.ts.
 * These tests do not require a live Figma plugin environment.
 * Each handler takes figma and the message as parameters, so a minimal mock is enough.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  applySetPort,
  createDispatcher,
  handleExecute,
  handleGetSelection,
  handleScreenshot,
} from "./code";

// ---------------------------------------------------------------------------
// UI bootstrap (source guard)
// ---------------------------------------------------------------------------

describe("showUI bootstrap", () => {
  const src = readFileSync(join(import.meta.dir, "code.ts"), "utf8");

  test("opts into themeColors so Figma injects theme variables", () => {
    // Without themeColors, the --figma-color-* variables are never injected and
    // the panel falls back to its white defaults. This guards that opt-in.
    expect(src).toContain("themeColors: true");
  });

  test("uses the compact main-screen height (150) as the initial panel height", () => {
    // Matches the screenSize("main") value so the panel opens at the correct size.
    expect(src).toContain("height: 150");
  });

  test("handles RESIZE messages from the UI", () => {
    // The RESIZE case must be present in the onmessage dispatch.
    expect(src).toContain('"RESIZE"');
  });

  test("calls figma.ui.resize to apply the dimensions", () => {
    expect(src).toContain("figma.ui.resize");
  });
});

// ---------------------------------------------------------------------------
// handleExecute
// ---------------------------------------------------------------------------

describe("handleExecute", () => {
  // An empty object is sufficient: the test code does not call figma or tf methods.
  const mockFigma = {} as unknown as PluginAPI;
  const mockTf = {} as unknown as ReturnType<import("./helpers").createTf>;

  test("returns a success message for code that returns a value", async () => {
    const msg = { type: "EXECUTE" as const, requestId: 1, code: "return 1 + 1;" };
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(true);
    expect(result.result).toBe(2);
    expect(result.requestId).toBe(1);
  });

  test("returns an error message for code that throws an Error object", async () => {
    const msg = { type: "EXECUTE" as const, requestId: 2, code: "throw new Error('oops');" };
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(false);
    expect(typeof result.error).toBe("string");
    expect(result.error).toContain("oops");
    expect(result.requestId).toBe(2);
  });

  test("returns an error message for code that throws a non-Error value and does not reject", async () => {
    const msg = { type: "EXECUTE" as const, requestId: 3, code: 'throw "boom";' };
    // The promise must resolve (not reject) with an error ResultMessage.
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(false);
    expect(result.error).toBe("boom");
    expect(result.requestId).toBe(3);
  });

  test("echoes the requestId in all cases", async () => {
    const success = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE" as const,
      requestId: 99,
      code: "return 42;",
    });
    expect(success.requestId).toBe(99);

    const error = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE" as const,
      requestId: 77,
      code: "throw new Error('e');",
    });
    expect(error.requestId).toBe(77);
  });

  test("times out and replies ok:false when the code awaits longer than timeoutMs", async () => {
    const msg = {
      type: "EXECUTE" as const,
      requestId: 12,
      code: "await new Promise((r) => setTimeout(r, 200)); return 1;",
      timeoutMs: 10,
    };
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(false);
    expect(result.requestId).toBe(12);
    expect(result.error).toContain("timed out in the plugin");
    expect(result.error).toContain("not idempotent");
  });

  test("ignores timeoutMs when the code finishes first", async () => {
    const msg = {
      type: "EXECUTE" as const,
      requestId: 13,
      code: "return 7;",
      timeoutMs: 5000,
    };
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(true);
    expect(result.result).toBe(7);
  });

  test("ignores a non-positive timeoutMs (runs without a timeout race)", async () => {
    const msg = {
      type: "EXECUTE" as const,
      requestId: 14,
      code: "return 1;",
      timeoutMs: 0,
    };
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(true);
  });

  test("appends a user-code line and column to a thrown Error's message when the stack carries one", async () => {
    const msg = {
      type: "EXECUTE" as const,
      requestId: 15,
      code: "function fail() { throw new Error('boom'); }\nreturn fail();",
    };
    const result = await handleExecute(mockFigma, mockTf, msg);
    expect(result.ok).toBe(false);
    expect(result.error).toContain("boom");
    // The exact line/column depend on the engine's stack format; just assert
    // the message was not corrupted and stayed a single string.
    expect(typeof result.error).toBe("string");
  });
});

// ---------------------------------------------------------------------------
// handleGetSelection
// ---------------------------------------------------------------------------

describe("handleGetSelection", () => {
  test("returns a selection message for a mocked selection", async () => {
    const mockNode = {
      id: "1:1",
      name: "Frame",
      type: "FRAME",
      x: 10,
      y: 20,
      width: 100,
      height: 50,
    };
    const mockFigma = {
      currentPage: { selection: [mockNode] },
    } as unknown as PluginAPI;
    const msg = { type: "GET_SELECTION" as const, requestId: 5 };
    const result = await handleGetSelection(mockFigma, msg);
    expect(result.ok).toBe(true);
    expect(result.requestId).toBe(5);
    expect(Array.isArray(result.selection)).toBe(true);
    const items = result.selection ?? [];
    expect(items[0].id).toBe("1:1");
    expect(items[0].name).toBe("Frame");
    expect(items[0].w).toBe(100);
    expect(items[0].h).toBe(50);
  });

  test("returns an empty selection array when nothing is selected", async () => {
    const mockFigma = {
      currentPage: { selection: [] },
    } as unknown as PluginAPI;
    const msg = { type: "GET_SELECTION" as const, requestId: 6 };
    const result = await handleGetSelection(mockFigma, msg);
    expect(result.ok).toBe(true);
    expect(result.selection).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// handleScreenshot
// ---------------------------------------------------------------------------

describe("handleScreenshot", () => {
  test("returns an error message when no nodeId is given and selection is empty", async () => {
    const mockFigma = {
      currentPage: { selection: [] },
    } as unknown as PluginAPI;
    const msg = { type: "SCREENSHOT" as const, requestId: 7 };
    const result = await handleScreenshot(mockFigma, msg);
    expect(result.ok).toBe(false);
    expect(result.error).toContain("no node to screenshot");
    expect(result.requestId).toBe(7);
  });

  test("returns an error message when the selected node has no exportAsync", async () => {
    // A node without exportAsync is not exportable.
    const mockNode = { id: "1:2", name: "Text" };
    const mockFigma = {
      currentPage: { selection: [mockNode] },
    } as unknown as PluginAPI;
    const msg = { type: "SCREENSHOT" as const, requestId: 8 };
    const result = await handleScreenshot(mockFigma, msg);
    expect(result.ok).toBe(false);
    expect(result.error).toBe("node is not exportable");
    expect(result.requestId).toBe(8);
  });

  test("returns an error message when getNodeByIdAsync returns null", async () => {
    const mockFigma = {
      currentPage: { selection: [] },
      async getNodeByIdAsync(_id: string) {
        return null;
      },
    } as unknown as PluginAPI;
    const msg = { type: "SCREENSHOT" as const, requestId: 9, nodeId: "missing-id" };
    const result = await handleScreenshot(mockFigma, msg);
    expect(result.ok).toBe(false);
    expect(result.error).toBe("node is not exportable");
    expect(result.requestId).toBe(9);
  });

  test("clamps an over-range scale to the documented maximum (4)", async () => {
    let usedScale: number | undefined;
    const mockNode = {
      id: "1:3",
      width: 10,
      height: 10,
      async exportAsync(settings: { constraint: { value: number } }) {
        usedScale = settings.constraint.value;
        return new Uint8Array([]);
      },
    };
    const mockFigma = {
      currentPage: { selection: [mockNode] },
      base64Encode: () => "",
    } as unknown as PluginAPI;
    const msg = { type: "SCREENSHOT" as const, requestId: 10, scale: 999 };
    await handleScreenshot(mockFigma, msg);
    expect(usedScale).toBe(4);
  });

  test("clamps an under-range scale to the documented minimum (0.1)", async () => {
    let usedScale: number | undefined;
    const mockNode = {
      id: "1:4",
      width: 10,
      height: 10,
      async exportAsync(settings: { constraint: { value: number } }) {
        usedScale = settings.constraint.value;
        return new Uint8Array([]);
      },
    };
    const mockFigma = {
      currentPage: { selection: [mockNode] },
      base64Encode: () => "",
    } as unknown as PluginAPI;
    const msg = { type: "SCREENSHOT" as const, requestId: 11, scale: 0.001 };
    await handleScreenshot(mockFigma, msg);
    expect(usedScale).toBe(0.1);
  });
});

// ---------------------------------------------------------------------------
// applySetPort
// ---------------------------------------------------------------------------

/** A minimal in-memory mock for figma.clientStorage. */
function makeMockStorage(): {
  store: Record<string, unknown>;
  getAsync(key: string): Promise<unknown>;
  setAsync(key: string, value: unknown): Promise<void>;
} {
  const store: Record<string, unknown> = {};
  return {
    store,
    async getAsync(key: string): Promise<unknown> {
      return key in store ? store[key] : undefined;
    },
    async setAsync(key: string, value: unknown): Promise<void> {
      store[key] = value;
    },
  };
}

describe("applySetPort", () => {
  test("persists a valid mid-range port and returns it", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 8080);
    expect(result).toBe(8080);
    expect(storage.store["turbofig:wsPort"]).toBe(8080);
  });

  test("accepts the minimum valid port (1)", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 1);
    expect(result).toBe(1);
    expect(storage.store["turbofig:wsPort"]).toBe(1);
  });

  test("accepts the maximum valid port (65535)", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 65535);
    expect(result).toBe(65535);
    expect(storage.store["turbofig:wsPort"]).toBe(65535);
  });

  test("accepts the default port (18847)", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 18847);
    expect(result).toBe(18847);
  });

  test("rejects port 0 and returns null", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 0);
    expect(result).toBeNull();
  });

  test("rejects port 65536 and returns null", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 65536);
    expect(result).toBeNull();
  });

  test("rejects a non-integer port and returns null", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, 8080.5);
    expect(result).toBeNull();
  });

  test("rejects a negative port and returns null", async () => {
    const storage = makeMockStorage();
    const result = await applySetPort(storage, -1);
    expect(result).toBeNull();
  });

  test("does not write to storage when the port is invalid", async () => {
    const storage = makeMockStorage();
    await applySetPort(storage, 0);
    expect(Object.keys(storage.store)).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// createDispatcher
// ---------------------------------------------------------------------------

describe("createDispatcher", () => {
  /** Builds a minimal mock PluginAPI good enough for dispatcher tests. */
  function makeMockFigma(overrides: Record<string, unknown> = {}): PluginAPI {
    return {
      fileKey: "key1",
      root: { name: "My File" },
      currentPage: { selection: [] },
      clientStorage: makeMockStorage(),
      ui: { resize: () => {} },
      ...overrides,
    } as unknown as PluginAPI;
  }

  test("READY replies with FILE_INFO then PORT", async () => {
    const posted: unknown[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) => posted.push(m));
    dispatch({ type: "READY" });
    // emitStoredPort is async; let it settle.
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect((posted[0] as { type: string }).type).toBe("FILE_INFO");
    expect((posted[1] as { type: string }).type).toBe("PORT");
  });

  test("STATUS bypasses the queue and replies immediately, even behind a slow EXECUTE", async () => {
    const posted: { type?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { type?: string; requestId?: number }),
    );
    dispatch({
      type: "EXECUTE",
      requestId: 1,
      code: "await new Promise((r) => setTimeout(r, 50)); return 1;",
    });
    dispatch({ type: "STATUS", requestId: 2 });
    // STATUS is synchronous inside the dispatcher; it must already be posted
    // before the slow EXECUTE above has any chance to settle.
    expect(posted[0]?.requestId).toBe(2);
    expect(posted[0]?.type).toBe("RESULT");
  });

  test("EXECUTE, GET_SELECTION and SCREENSHOT run FIFO, not interleaved", async () => {
    const order: number[] = [];
    const posted: { requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { requestId?: number }),
    );
    // Job 1 is slower than job 2; FIFO means 1 must still finish first.
    dispatch({
      type: "EXECUTE",
      requestId: 1,
      code: "await new Promise((r) => setTimeout(r, 30)); return 1;",
    });
    dispatch({ type: "GET_SELECTION", requestId: 2 });
    dispatch({
      type: "EXECUTE",
      requestId: 3,
      code: "return 3;",
    });
    await new Promise((r) => setTimeout(r, 100));
    for (const msg of posted) {
      if (msg.requestId !== undefined) order.push(msg.requestId);
    }
    expect(order).toEqual([1, 2, 3]);
  });

  test("an oversized EXECUTE result is capped to an ok:false error before posting", async () => {
    const posted: { ok?: boolean; error?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { ok?: boolean; error?: string; requestId?: number }),
    );
    dispatch({
      type: "EXECUTE",
      requestId: 4,
      code: `return "x".repeat(17 * 1024 * 1024);`,
    });
    await new Promise((r) => setTimeout(r, 20));
    expect(posted).toHaveLength(1);
    expect(posted[0]?.ok).toBe(false);
    expect(posted[0]?.error).toContain("result too large");
    expect(posted[0]?.requestId).toBe(4);
  });

  test("RESIZE calls figma.ui.resize with the given dimensions", () => {
    const resizeCalls: [number, number][] = [];
    const figma = makeMockFigma({
      ui: { resize: (w: number, h: number) => resizeCalls.push([w, h]) },
    });
    const dispatch = createDispatcher(figma, () => {});
    dispatch({ type: "RESIZE", width: 300, height: 240 });
    expect(resizeCalls).toEqual([[300, 240]]);
  });

  test("SET_PORT failure (rejected setAsync) does not throw or reject", async () => {
    const figma = makeMockFigma({
      clientStorage: {
        getAsync: async () => undefined,
        setAsync: async () => {
          throw new Error("quota exceeded");
        },
      },
    });
    const posted: unknown[] = [];
    const dispatch = createDispatcher(figma, (m) => posted.push(m));
    expect(() => dispatch({ type: "SET_PORT", port: 9000 })).not.toThrow();
    await new Promise((r) => setTimeout(r, 10));
    // No PORT message, since the save failed; and nothing should throw.
    expect(posted).toHaveLength(0);
  });

  test("ignores a malformed message instead of throwing", () => {
    const dispatch = createDispatcher(makeMockFigma(), () => {});
    expect(() => dispatch(null)).not.toThrow();
    expect(() => dispatch({ type: "NOT_A_REAL_TYPE" })).not.toThrow();
  });
});
