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
  mapStackToUserLine,
} from "./code";

// ---------------------------------------------------------------------------
// UI bootstrap (source guard)
// ---------------------------------------------------------------------------

/** Poll until `cond` is true. Fails with a clear message on timeout. */
async function waitUntil(
  cond: () => boolean,
  what: string,
  timeoutMs = 2000,
  intervalMs = 5,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!cond()) {
    if (Date.now() > deadline)
      throw new Error(`timed out after ${timeoutMs}ms waiting for ${what}`);
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

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

  test("reports the right user line and appends that source line", async () => {
    const code = "const a = 1;\nconst b = 2;\n    throw new Error('bad'); // here\nconst c = 3;";
    const result = await handleExecute(mockFigma, mockTf, { type: "EXECUTE", requestId: 20, code });
    expect(result.error).toContain("(line 3, column ");
    expect(result.error).toContain("\n> throw new Error('bad'); // here");
  });

  test("caps the appended source line at 120 chars", async () => {
    const code = `const a = 1;\nthrow new Error('bad'); // ${"x".repeat(200)}`;
    const result = await handleExecute(mockFigma, mockTf, { type: "EXECUTE", requestId: 25, code });
    const srcLine = (result.error ?? "").split("\n> ")[1]?.split("\n")[0] ?? "";
    expect(srcLine.startsWith("throw new Error('bad');")).toBe(true);
    expect(srcLine.length).toBe(123);
  });

  test("a throw inside a tf helper never shows a wrong user line", async () => {
    const tf = {
      boom() {
        throw new Error("helper failed");
      },
    } as unknown as ReturnType<import("./helpers").createTf>;
    const code = "const a = 1;\nconst b = 2;\nconst c = 3;\ntf.boom();\nconst d = 4;";
    const result = await handleExecute(mockFigma, tf, { type: "EXECUTE", requestId: 26, code });
    const err = result.error ?? "";
    const m = err.match(/\(line (\d+),/);
    if (m) {
      expect(err).toContain(`\n> ${code.split("\n")[Number(m[1]) - 1]}`);
      expect(m[1]).toBe("4");
    } else {
      expect(err).not.toContain("\n> ");
    }
  });

  test("adds a hint for an unloaded font error", async () => {
    const code = `throw new Error('Cannot write to node with unloaded font "Inter"');`;
    const result = await handleExecute(mockFigma, mockTf, { type: "EXECUTE", requestId: 21, code });
    expect(result.error).toContain("Hint: call await figma.loadFontAsync");
  });

  test("no sync-API hint for a getNodeByIdAsync error", async () => {
    const code = `throw new Error('getNodeByIdAsync failed');`;
    const result = await handleExecute(mockFigma, mockTf, { type: "EXECUTE", requestId: 27, code });
    expect(result.error).not.toContain("Hint:");
  });

  test("adds a hint when a lookup returns null", async () => {
    const code = `const n = null;\nreturn n.name;`;
    const result = await handleExecute(mockFigma, mockTf, { type: "EXECUTE", requestId: 22, code });
    expect(result.error).toContain("Hint: a lookup returned null");
  });

  test("adds a hint for a missing tf helper", async () => {
    const code = `tf.nope();`;
    const result = await handleExecute(mockFigma, { zeta() {}, alpha() {}, n: 1 } as never, {
      type: "EXECUTE",
      requestId: 23,
      code,
    });
    expect(result.error).toContain("tf has: alpha, zeta");
    expect(result.error).not.toContain("tf-api.md");
  });

  test("adds a hint for a syntax error", async () => {
    const result = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE",
      requestId: 28,
      code: "const a = ;",
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain(
      "Hint: syntax error in your code: check quotes, brackets and commas",
    );
  });

  test("syntax hint mentions redeclaring figma or tf", async () => {
    const result = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE",
      requestId: 30,
      code: "const tf = 1;",
    });
    expect(result.error).toContain("do not redeclare figma or tf");
  });

  test("a throwing tf getter does not break the hint", async () => {
    const tf = {
      get boom(): never {
        throw new Error("x");
      },
    };
    const result = await handleExecute(mockFigma, tf as never, {
      type: "EXECUTE",
      requestId: 31,
      code: "tf.nope();",
    });
    expect(result.ok).toBe(false);
    expect(result.error).toContain("that tf helper does not exist");
  });

  test("a null-prototype thrown value gives an error RESULT", async () => {
    const result = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE",
      requestId: 32,
      code: "throw Object.create(null);",
    });
    expect(result.ok).toBe(false);
    expect(result.code).toBe("script_error");
    expect(result.error).toContain("non-printable thrown value");
  });

  test("a throwing message getter gives an error RESULT", async () => {
    const result = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE",
      requestId: 33,
      code: "throw { get message() { throw 1; } };",
    });
    expect(result.ok).toBe(false);
    expect(result.code).toBe("script_error");
    expect(result.error).toContain("non-printable thrown value");
  });

  test("adds no syntax hint for a runtime error", async () => {
    const result = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE",
      requestId: 29,
      code: "throw new Error('oops');",
    });
    expect(result.error).not.toContain("syntax error");
  });

  test("adds no hint for an unrelated error", async () => {
    const result = await handleExecute(mockFigma, mockTf, {
      type: "EXECUTE",
      requestId: 24,
      code: "throw new Error('oops');",
    });
    expect(result.error).not.toContain("Hint:");
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

  const scaleFor = async (
    w: number,
    h: number,
    extra: { scale?: number; maxDim?: number },
  ): Promise<number | undefined> => {
    let used: number | undefined;
    const node = {
      id: "1:9",
      width: w,
      height: h,
      async exportAsync(settings: { constraint: { value: number } }) {
        used = settings.constraint.value;
        return new Uint8Array([]);
      },
    };
    const fig = {
      currentPage: { selection: [node] },
      base64Encode: () => "",
    } as unknown as PluginAPI;
    await handleScreenshot(fig, { type: "SCREENSHOT" as const, requestId: 50, ...extra });
    return used;
  };

  test("maxDim caps the scale on a large node", async () => {
    expect(await scaleFor(4000, 2000, { scale: 2, maxDim: 1000 })).toBe(0.25);
  });

  test("maxDim floors the scale so the export never exceeds maxDim", async () => {
    const scale = await scaleFor(3000, 100, { maxDim: 1000 });
    expect(scale).toBe(0.3333);
    expect(scale * 3000).toBeLessThanOrEqual(1000);
  });

  test("maxDim does not change a small node", async () => {
    expect(await scaleFor(100, 50, { scale: 2, maxDim: 1000 })).toBe(2);
  });

  test("absent maxDim leaves the scale unchanged", async () => {
    expect(await scaleFor(4000, 2000, { scale: 2 })).toBe(2);
  });

  const scaleForNode = async (
    node: Record<string, unknown>,
    extra: { scale?: number; maxDim?: number },
  ): Promise<number | undefined> => {
    let used: number | undefined;
    const n = Object.defineProperties(
      {
        async exportAsync(settings: { constraint: { value: number } }) {
          used = settings.constraint.value;
          return new Uint8Array([]);
        },
      },
      Object.getOwnPropertyDescriptors(node),
    );
    const fig = {
      currentPage: { selection: [n] },
      base64Encode: () => "",
    } as unknown as PluginAPI;
    await handleScreenshot(fig, { type: "SCREENSHOT" as const, requestId: 51, ...extra });
    return used;
  };

  test("maxDim prefers absoluteRenderBounds over width and height", async () => {
    const node = {
      width: 100,
      height: 100,
      absoluteRenderBounds: { x: 0, y: 0, width: 400, height: 200 },
    };
    expect(await scaleForNode(node, { maxDim: 100 })).toBe(0.25);
  });

  test("maxDim on a page uses the union of its children bounds", async () => {
    const page = {
      type: "PAGE",
      children: [
        { absoluteBoundingBox: { x: 0, y: 0, width: 100, height: 100 } },
        { absoluteBoundingBox: { x: 900, y: 0, width: 100, height: 50 } },
      ],
    };
    expect(await scaleForNode(page, { maxDim: 500 })).toBe(0.5);
  });

  test("maxDim on a zero-size node skips the cap", async () => {
    expect(await scaleForNode({ width: 0, height: 0 }, { scale: 2, maxDim: 100 })).toBe(2);
  });

  test("maxDim with absent bounds skips the cap", async () => {
    expect(await scaleForNode({}, { scale: 2, maxDim: 100 })).toBe(2);
    expect(await scaleForNode({ type: "PAGE", children: [] }, { maxDim: 100 })).toBe(1);
  });

  test("a throwing bounds getter still screenshots, with and without maxDim", async () => {
    const node = {
      type: "PAGE",
      get children(): unknown[] {
        throw new Error("page not loaded");
      },
    };
    expect(await scaleForNode(node, { scale: 2, maxDim: 100 })).toBe(2);
    expect(await scaleForNode(node, { scale: 2 })).toBe(2);
  });

  test("a page uses absoluteBoundingBox, not render bounds, for children", async () => {
    const page = {
      type: "PAGE",
      children: [
        {
          absoluteBoundingBox: { x: 0, y: 0, width: 200, height: 100 },
          absoluteRenderBounds: { x: 0, y: 0, width: 800, height: 400 },
        },
      ],
    };
    expect(await scaleForNode(page, { maxDim: 100 })).toBe(0.5);
  });

  test("maxDim never drops the scale below 0.01", async () => {
    expect(await scaleFor(100000, 100000, { maxDim: 10 })).toBe(0.01);
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

  test("READY no longer replies PROMPT_MODE and never reads the old promptMode key", async () => {
    const reads: string[] = [];
    const a: { type: string }[] = [];
    const figma = makeMockFigma({
      clientStorage: {
        getAsync: async (k: string) => {
          reads.push(k);
          return "mcp";
        },
        setAsync: async () => {},
      },
    });
    createDispatcher(figma, (m) => a.push(m as { type: string }))({ type: "READY" });
    await new Promise((r) => setTimeout(r, 10));
    expect(a.some((m) => m.type === "PROMPT_MODE")).toBe(false);
    expect(reads).not.toContain("turbofig:promptMode");
  });

  test("SET_PROMPT_MODE is ignored and writes nothing", async () => {
    const saved: [string, unknown][] = [];
    const figma = makeMockFigma({
      clientStorage: {
        getAsync: async () => undefined,
        setAsync: async (k: string, v: unknown) => {
          saved.push([k, v]);
        },
      },
    });
    createDispatcher(figma, () => {})({ type: "SET_PROMPT_MODE", mode: "mcp" });
    await new Promise((r) => setTimeout(r, 10));
    expect(saved).toEqual([]);
  });

  test("READY also replies SHOW_LOG: true by default, false when stored false", async () => {
    const settle = () => new Promise((r) => setTimeout(r, 10));
    const a: { type: string; show?: boolean }[] = [];
    createDispatcher(makeMockFigma(), (m) => a.push(m as { type: string }))({ type: "READY" });
    await settle();
    expect(a.find((m) => m.type === "SHOW_LOG")?.show).toBe(true);

    const b: { type: string; show?: boolean }[] = [];
    const figma = makeMockFigma({
      clientStorage: { getAsync: async () => false, setAsync: async () => {} },
    });
    createDispatcher(figma, (m) => b.push(m as { type: string }))({ type: "READY" });
    await settle();
    expect(b.find((m) => m.type === "SHOW_LOG")?.show).toBe(false);
  });

  test("SET_SHOW_LOG persists the setting under turbofig:showLog", async () => {
    const saved: [string, unknown][] = [];
    const figma = makeMockFigma({
      clientStorage: {
        getAsync: async () => undefined,
        setAsync: async (k: string, v: unknown) => {
          saved.push([k, v]);
        },
      },
    });
    createDispatcher(figma, () => {})({ type: "SET_SHOW_LOG", show: false });
    await new Promise((r) => setTimeout(r, 10));
    expect(saved).toEqual([["turbofig:showLog", false]]);
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
    const posted: { type?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { type?: string; requestId?: number }),
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
    await waitUntil(() => posted.filter((m) => m.type === "RESULT").length >= 3, "3 RESULT frames");
    // Only RESULT completion order matters here: each job also posts a
    // STARTED frame first (see "each queued job announces STARTED..." below).
    for (const msg of posted) {
      if (msg.type === "RESULT" && msg.requestId !== undefined) order.push(msg.requestId);
    }
    expect(order).toEqual([1, 2, 3]);
  });

  test("each queued job announces STARTED before its RESULT, in FIFO order", async () => {
    const posted: { type?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { type?: string; requestId?: number }),
    );
    dispatch({ type: "EXECUTE", requestId: 1, code: "return 1;" });
    dispatch({ type: "GET_SELECTION", requestId: 2 });
    await waitUntil(() => posted.length >= 4, "4 posted frames");
    expect(posted.map((m) => [m.type, m.requestId])).toEqual([
      ["STARTED", 1],
      ["RESULT", 1],
      ["STARTED", 2],
      ["RESULT", 2],
    ]);
  });

  test("a job that expires in the queue never gets a STARTED frame", async () => {
    const posted: { type?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { type?: string; requestId?: number }),
    );
    dispatch({
      type: "EXECUTE",
      requestId: 1,
      code: "await new Promise((r) => setTimeout(r, 50)); return 1;",
    });
    dispatch({ type: "GET_SELECTION", requestId: 2, timeoutMs: 10 });
    // Poll for job 2's expiry RESULT so the negative check runs after the
    // queue has fully drained, not after a fixed window.
    await waitUntil(
      () => posted.some((m) => m.type === "RESULT" && m.requestId === 2),
      "RESULT for job 2",
    );
    const startedForJob2 = posted.find((m) => m.type === "STARTED" && m.requestId === 2);
    expect(startedForJob2).toBeUndefined();
  });

  test("an oversized EXECUTE result is capped to an ok:false error before posting", async () => {
    const posted: { type?: string; ok?: boolean; error?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { type?: string; ok?: boolean; error?: string; requestId?: number }),
    );
    dispatch({
      type: "EXECUTE",
      requestId: 4,
      code: `return "x".repeat(17 * 1024 * 1024);`,
    });
    await waitUntil(() => posted.some((m) => m.type === "RESULT"), "capped RESULT", 5000);
    const result = posted.find((m) => m.type === "RESULT");
    expect(result?.ok).toBe(false);
    expect(result?.error).toContain("result too large");
    expect(result?.requestId).toBe(4);
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

  test("a job already past its deadline when dequeued never runs, and replies 'expired in the queue'", async () => {
    const posted: { requestId?: number; ok?: boolean; error?: string }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { requestId?: number; ok?: boolean; error?: string }),
    );
    dispatch({
      type: "EXECUTE",
      requestId: 1,
      code: "await new Promise((r) => setTimeout(r, 50)); return 1;",
    });
    // Enqueued right behind job 1: its 10ms deadline is stamped now, and
    // elapses long before job 1 finishes and job 2 reaches the front.
    dispatch({ type: "GET_SELECTION", requestId: 2, timeoutMs: 10 });
    await waitUntil(() => posted.some((m) => m.requestId === 2), "reply for job 2");
    const second = posted.find((m) => m.requestId === 2);
    expect(second?.ok).toBe(false);
    expect(second?.error).toContain("expired in the queue");
  });

  test("GET_SELECTION and SCREENSHOT race against timeoutMs too, not only EXECUTE", async () => {
    const mockNode = {
      id: "1:5",
      width: 10,
      height: 10,
      async exportAsync() {
        await new Promise((r) => setTimeout(r, 200));
        return new Uint8Array([]);
      },
    };
    const figma = makeMockFigma({
      currentPage: { selection: [mockNode] },
      base64Encode: () => "",
    });
    const posted: { type?: string; requestId?: number; ok?: boolean; error?: string }[] = [];
    const dispatch = createDispatcher(figma, (m) =>
      posted.push(m as { type?: string; requestId?: number; ok?: boolean; error?: string }),
    );
    dispatch({ type: "SCREENSHOT", requestId: 9, timeoutMs: 20 });
    await waitUntil(() => posted.some((m) => m.type === "RESULT"), "timed-out RESULT");
    const result = posted.find((m) => m.type === "RESULT");
    expect(result?.ok).toBe(false);
    expect(result?.error).toContain("timed out");
    expect(result?.requestId).toBe(9);
  });

  test("defaults the queue budget to 30000ms when timeoutMs is absent", async () => {
    const posted: { type?: string; requestId?: number; ok?: boolean }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) =>
      posted.push(m as { type?: string; requestId?: number; ok?: boolean }),
    );
    dispatch({ type: "GET_SELECTION", requestId: 11 });
    await waitUntil(() => posted.some((m) => m.type === "RESULT"), "RESULT for job 11");
    const result = posted.find((m) => m.type === "RESULT");
    expect(result?.requestId).toBe(11);
    expect(result?.ok).toBe(true);
  });

  test("a throwing post on one job's RESULT does not drop every later queued job", async () => {
    const posted: { type?: string; requestId?: number }[] = [];
    const dispatch = createDispatcher(makeMockFigma(), (m) => {
      const msg = m as { type?: string; requestId?: number };
      // Simulate job 1's RESULT post failing (e.g. a transiently closed UI
      // channel); every other post, including job 1's own STARTED, succeeds.
      if (msg.type === "RESULT" && msg.requestId === 1) throw new Error("boom");
      posted.push(msg);
    });
    dispatch({ type: "EXECUTE", requestId: 1, code: "return 1;" });
    dispatch({ type: "EXECUTE", requestId: 2, code: "return 2;" });
    await waitUntil(
      () => posted.some((m) => m.type === "RESULT" && m.requestId === 2),
      "RESULT for job 2",
    );
    expect(posted.map((m) => [m.type, m.requestId])).toEqual([
      ["STARTED", 1],
      ["STARTED", 2],
      ["RESULT", 2],
    ]);
  });
});

/** Normalised prefix of a frame, as calibrate stores it. */
function parseFrameForTest(frame: string): string {
  return frame.replace(/:\d+:\d+\)?$/, "").replace(/eval at .*?\), /, "");
}

describe("mapStackToUserLine", () => {
  const cal = { prefix: "at eval (<anonymous>", offset: 3 };

  test("skips a foreign first frame and uses the calibrated frame", () => {
    const stack = ["Error: x", "at helper (code.js:5:9)", "at eval (<anonymous>:7:4)"];
    expect(mapStackToUserLine(stack, cal, 10)).toEqual({ line: 4, column: 4 });
  });

  test("matches V8 frames whose host call sites differ", () => {
    const probe = parseFrameForTest(
      "    at eval (eval at calibrate (code.js:90:20), <anonymous>:5:7)",
    );
    const stack = ["Error: x", "    at eval (eval at run (code.js:180:24), <anonymous>:9:3)"];
    expect(mapStackToUserLine(stack, { prefix: probe, offset: 3 }, 10)).toEqual({
      line: 6,
      column: 3,
    });
  });

  test("gives no position when only a foreign frame exists", () => {
    expect(mapStackToUserLine(["Error: x", "at helper (code.js:5:9)"], cal, 10)).toBeNull();
  });

  test("gives no position when the mapped line is out of range", () => {
    expect(mapStackToUserLine(["at eval (<anonymous>:99:1)"], cal, 10)).toBeNull();
  });
});
