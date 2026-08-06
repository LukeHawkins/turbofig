/**
 * Unit tests for the exported handler functions in code.ts.
 * These tests do not require a live Figma plugin environment.
 * Each handler takes figma and the message as parameters, so a minimal mock is enough.
 */

import { describe, expect, test } from "bun:test";
import { handleExecute, handleGetSelection, handleScreenshot } from "./code";

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
});
