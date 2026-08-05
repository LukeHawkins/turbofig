import { describe, expect, test } from "bun:test";
import {
  backoffDelayMs,
  buildExecuteError,
  buildExecuteSuccess,
  buildResult,
  buildScreenshot,
  buildSelection,
  DEPRECATION_PREAMBLE,
  isDaemonMessage,
  MAX_SELECTION_DEPTH,
  safeResult,
  serializeNode,
  toSelectionItem,
  wrapUserCode,
} from "./protocol";

describe("isDaemonMessage", () => {
  test("accepts a valid FILE_INFO message", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", fileKey: "abc123", name: "My File" })).toBe(true);
  });

  test("accepts a valid STATUS message", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: 1 })).toBe(true);
  });

  test("accepts a valid RESULT message", () => {
    expect(isDaemonMessage({ type: "RESULT", requestId: 1 })).toBe(true);
  });

  test("accepts a RESULT message with extra fields", () => {
    expect(
      isDaemonMessage({ type: "RESULT", requestId: 2, ok: true, fileKey: "f", name: "n" }),
    ).toBe(true);
  });

  test("rejects null", () => {
    expect(isDaemonMessage(null)).toBe(false);
  });

  test("rejects a plain string", () => {
    expect(isDaemonMessage("FILE_INFO")).toBe(false);
  });

  test("rejects an unknown type", () => {
    expect(isDaemonMessage({ type: "EXECUTE" })).toBe(false);
  });

  test("rejects FILE_INFO missing fileKey", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", name: "My File" })).toBe(false);
  });

  test("rejects FILE_INFO missing name", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", fileKey: "abc123" })).toBe(false);
  });

  test("rejects STATUS with a string requestId", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: "req-1" })).toBe(false);
  });

  test("rejects STATUS missing requestId", () => {
    expect(isDaemonMessage({ type: "STATUS" })).toBe(false);
  });

  test("rejects an empty object", () => {
    expect(isDaemonMessage({})).toBe(false);
  });
});

describe("buildResult", () => {
  test("echoes the requestId from the STATUS message", () => {
    const status = { type: "STATUS" as const, requestId: 42 };
    const result = buildResult(status, "key1", "My File");
    expect(result.requestId).toBe(42);
  });

  test("sets type to RESULT and ok to true", () => {
    const status = { type: "STATUS" as const, requestId: 7 };
    const result = buildResult(status, "key2", "Other File");
    expect(result.type).toBe("RESULT");
    expect(result.ok).toBe(true);
  });

  test("includes the supplied fileKey and name", () => {
    const status = { type: "STATUS" as const, requestId: 1 };
    const result = buildResult(status, "abc", "Design System");
    expect(result.fileKey).toBe("abc");
    expect(result.name).toBe("Design System");
  });

  test("result passes the isDaemonMessage guard", () => {
    const status = { type: "STATUS" as const, requestId: 99 };
    const result = buildResult(status, "f", "n");
    expect(isDaemonMessage(result)).toBe(true);
  });
});

describe("isDaemonMessage: EXECUTE branch", () => {
  test("accepts a valid EXECUTE message", () => {
    expect(isDaemonMessage({ type: "EXECUTE", requestId: 1, code: "return 42;" })).toBe(true);
  });

  test("rejects EXECUTE missing code", () => {
    expect(isDaemonMessage({ type: "EXECUTE", requestId: 1 })).toBe(false);
  });

  test("rejects EXECUTE with non-string code", () => {
    expect(isDaemonMessage({ type: "EXECUTE", requestId: 1, code: 99 })).toBe(false);
  });

  test("rejects EXECUTE missing requestId", () => {
    expect(isDaemonMessage({ type: "EXECUTE", code: "return 1;" })).toBe(false);
  });
});

describe("buildExecuteSuccess", () => {
  test("returns a RESULT with ok true and the given result", () => {
    const msg = buildExecuteSuccess(7, { x: 1 });
    expect(msg.type).toBe("RESULT");
    expect(msg.requestId).toBe(7);
    expect(msg.ok).toBe(true);
    expect(msg.result).toEqual({ x: 1 });
  });

  test("echoes the requestId", () => {
    const msg = buildExecuteSuccess(99, null);
    expect(msg.requestId).toBe(99);
  });

  test("result passes the isDaemonMessage guard", () => {
    expect(isDaemonMessage(buildExecuteSuccess(1, "hello"))).toBe(true);
  });
});

describe("buildExecuteError", () => {
  test("returns a RESULT with ok false and the error string", () => {
    const msg = buildExecuteError(3, "something went wrong");
    expect(msg.type).toBe("RESULT");
    expect(msg.requestId).toBe(3);
    expect(msg.ok).toBe(false);
    expect(msg.error).toBe("something went wrong");
  });

  test("echoes the requestId", () => {
    const msg = buildExecuteError(42, "oops");
    expect(msg.requestId).toBe(42);
  });

  test("result passes the isDaemonMessage guard", () => {
    expect(isDaemonMessage(buildExecuteError(1, "err"))).toBe(true);
  });
});

describe("safeResult", () => {
  test("a plain object round-trips through JSON", () => {
    const input = { a: 1, b: "hello", c: true };
    expect(safeResult(input)).toEqual(input);
  });

  test("a value with a circular reference falls back to String()", () => {
    const obj: Record<string, unknown> = {};
    obj.self = obj;
    const result = safeResult(obj);
    expect(typeof result).toBe("string");
  });

  test("a primitive value round-trips", () => {
    expect(safeResult(42)).toBe(42);
    expect(safeResult("hello")).toBe("hello");
    expect(safeResult(null)).toBe(null);
  });
});

describe("deprecation preamble", () => {
  test("preamble sets strict mode first", () => {
    expect(DEPRECATION_PREAMBLE.startsWith('"use strict";')).toBe(true);
  });

  test("preamble lists key async replacements", () => {
    expect(DEPRECATION_PREAMBLE).toContain("getNodeByIdAsync");
    expect(DEPRECATION_PREAMBLE).toContain("getLocalPaintStylesAsync");
    expect(DEPRECATION_PREAMBLE).toContain("setCurrentPageAsync");
  });

  test("wrapUserCode puts the preamble before the user code", () => {
    const wrapped = wrapUserCode("return 1;");
    expect(wrapped.startsWith(DEPRECATION_PREAMBLE)).toBe(true);
    expect(wrapped.endsWith("return 1;")).toBe(true);
    expect(wrapped.indexOf(DEPRECATION_PREAMBLE)).toBeLessThan(wrapped.indexOf("return 1;"));
  });

  test("wrapUserCode keeps the user code intact", () => {
    const code = "const x = await figma.getNodeByIdAsync('1');\nreturn x;";
    expect(wrapUserCode(code)).toContain(code);
  });
});

describe("isDaemonMessage: GET_SELECTION branch", () => {
  test("accepts a valid GET_SELECTION message", () => {
    expect(isDaemonMessage({ type: "GET_SELECTION", requestId: 5 })).toBe(true);
  });

  test("rejects GET_SELECTION missing requestId", () => {
    expect(isDaemonMessage({ type: "GET_SELECTION" })).toBe(false);
  });

  test("rejects GET_SELECTION with a string requestId", () => {
    expect(isDaemonMessage({ type: "GET_SELECTION", requestId: "req-1" })).toBe(false);
  });
});

describe("toSelectionItem", () => {
  test("maps a full node object correctly", () => {
    const node = {
      id: "1:2",
      name: "Frame 1",
      type: "FRAME",
      x: 10,
      y: 20,
      width: 100,
      height: 50,
    };
    const item = toSelectionItem(node);
    expect(item.id).toBe("1:2");
    expect(item.name).toBe("Frame 1");
    expect(item.type).toBe("FRAME");
    expect(item.x).toBe(10);
    expect(item.y).toBe(20);
    expect(item.w).toBe(100);
    expect(item.h).toBe(50);
  });

  test("falls back to 0 when width, height, x, y are missing", () => {
    const node = { id: "1:3", name: "Box", type: "RECTANGLE" };
    const item = toSelectionItem(node);
    expect(item.x).toBe(0);
    expect(item.y).toBe(0);
    expect(item.w).toBe(0);
    expect(item.h).toBe(0);
  });

  test("falls back to empty string when name and type are missing", () => {
    const node = { id: "1:4" };
    const item = toSelectionItem(node);
    expect(item.name).toBe("");
    expect(item.type).toBe("");
  });

  test("falls back to empty string when id is missing", () => {
    const node = { name: "No ID" };
    const item = toSelectionItem(node);
    expect(item.id).toBe("");
  });
});

describe("buildSelection", () => {
  test("returns a RESULT with ok true and the selection array", () => {
    const items = [{ id: "1:1", name: "A", type: "FRAME", x: 0, y: 0, w: 10, h: 10 }];
    const msg = buildSelection(7, items);
    expect(msg.type).toBe("RESULT");
    expect(msg.requestId).toBe(7);
    expect(msg.ok).toBe(true);
    expect(msg.selection).toEqual(items);
  });

  test("echoes the requestId", () => {
    const msg = buildSelection(42, []);
    expect(msg.requestId).toBe(42);
  });

  test("result passes the isDaemonMessage guard", () => {
    const msg = buildSelection(1, []);
    expect(isDaemonMessage(msg)).toBe(true);
  });

  test("an empty selection produces an empty array", () => {
    const msg = buildSelection(3, []);
    expect(msg.selection).toEqual([]);
  });
});

describe("backoffDelayMs", () => {
  test("attempt 0 returns the base delay of 500ms", () => {
    expect(backoffDelayMs(0)).toBe(500);
  });

  test("attempt 1 returns 1000ms (doubles each step)", () => {
    expect(backoffDelayMs(1)).toBe(1000);
  });

  test("attempt 2 returns 2000ms", () => {
    expect(backoffDelayMs(2)).toBe(2000);
  });

  test("grows monotonically until the cap", () => {
    let prev = backoffDelayMs(0);
    for (let i = 1; i <= 6; i++) {
      const curr = backoffDelayMs(i);
      expect(curr).toBeGreaterThanOrEqual(prev);
      prev = curr;
    }
  });

  test("caps at 30000ms for attempt 6 and beyond", () => {
    // 500 * 2^6 = 32000 > 30000, so attempt 6 must be capped
    expect(backoffDelayMs(6)).toBe(30000);
    expect(backoffDelayMs(100)).toBe(30000);
  });

  test("never exceeds the 30000ms cap", () => {
    for (let i = 0; i <= 50; i++) {
      expect(backoffDelayMs(i)).toBeLessThanOrEqual(30000);
    }
  });
});

describe("isDaemonMessage: SCREENSHOT branch", () => {
  test("accepts a valid SCREENSHOT message with only requestId", () => {
    expect(isDaemonMessage({ type: "SCREENSHOT", requestId: 1 })).toBe(true);
  });

  test("accepts a valid SCREENSHOT message with scale and nodeId", () => {
    expect(isDaemonMessage({ type: "SCREENSHOT", requestId: 2, scale: 2, nodeId: "1:2" })).toBe(
      true,
    );
  });

  test("rejects SCREENSHOT missing requestId", () => {
    expect(isDaemonMessage({ type: "SCREENSHOT" })).toBe(false);
  });

  test("rejects SCREENSHOT with a string requestId", () => {
    expect(isDaemonMessage({ type: "SCREENSHOT", requestId: "req-1" })).toBe(false);
  });
});

describe("serializeNode", () => {
  const baseNode = {
    id: "1:1",
    name: "Frame",
    type: "FRAME",
    x: 10,
    y: 20,
    width: 100,
    height: 50,
  };

  test("base shape equals the seven fields with no fields/depth", () => {
    const result = serializeNode(baseNode, undefined, 0);
    expect(result.id).toBe("1:1");
    expect(result.name).toBe("Frame");
    expect(result.type).toBe("FRAME");
    expect(result.x).toBe(10);
    expect(result.y).toBe(20);
    expect(result.w).toBe(100);
    expect(result.h).toBe(50);
    expect(Object.keys(result)).toHaveLength(7);
  });

  test("fields adds requested extra props and ignores missing ones", () => {
    const node = { ...baseNode, visible: true, opacity: 0.5 };
    const result = serializeNode(node, ["visible", "opacity", "notPresent"], 0);
    expect(result.visible).toBe(true);
    expect(result.opacity).toBe(0.5);
    expect("notPresent" in result).toBe(false);
  });

  test("depth 1 includes a children array one level deep", () => {
    const child = { id: "1:2", name: "Child", type: "TEXT", x: 0, y: 0, width: 10, height: 10 };
    const node = { ...baseNode, children: [child] };
    const result = serializeNode(node, undefined, 1);
    expect(Array.isArray(result.children)).toBe(true);
    const kids = result.children as (typeof result)[];
    expect(kids).toHaveLength(1);
    expect(kids[0].id).toBe("1:2");
  });

  test("depth 0 omits children entirely", () => {
    const child = { id: "1:2", name: "Child", type: "TEXT", x: 0, y: 0, width: 10, height: 10 };
    const node = { ...baseNode, children: [child] };
    const result = serializeNode(node, undefined, 0);
    expect("children" in result).toBe(false);
  });

  test("depth clamps at MAX_SELECTION_DEPTH (pass 99, assert it stops at the cap)", () => {
    // Build a node tree that is MAX_SELECTION_DEPTH + 2 levels deep.
    let deepest: Record<string, unknown> = {
      id: "leaf",
      name: "Leaf",
      type: "RECTANGLE",
      x: 0,
      y: 0,
      width: 1,
      height: 1,
    };
    for (let i = 0; i < MAX_SELECTION_DEPTH + 1; i++) {
      deepest = {
        id: `n${i}`,
        name: `Level ${i}`,
        type: "FRAME",
        x: 0,
        y: 0,
        width: 10,
        height: 10,
        children: [deepest],
      };
    }
    const result = serializeNode(deepest, undefined, 99);
    // Traverse the result and count actual depth.
    let current: Record<string, unknown> = result;
    let actualDepth = 0;
    while (Array.isArray(current.children) && current.children.length > 0) {
      current = (current.children as Record<string, unknown>[])[0];
      actualDepth++;
    }
    expect(actualDepth).toBe(MAX_SELECTION_DEPTH);
  });
});

describe("buildScreenshot", () => {
  test("returns a RESULT with ok true, png, w, and h", () => {
    const msg = buildScreenshot(5, "aGVsbG8=", 100, 50);
    expect(msg.type).toBe("RESULT");
    expect(msg.requestId).toBe(5);
    expect(msg.ok).toBe(true);
    expect(msg.png).toBe("aGVsbG8=");
    expect(msg.w).toBe(100);
    expect(msg.h).toBe(50);
  });

  test("echoes the requestId", () => {
    const msg = buildScreenshot(42, "abc", 10, 20);
    expect(msg.requestId).toBe(42);
  });

  test("result passes the isDaemonMessage guard", () => {
    expect(isDaemonMessage(buildScreenshot(1, "abc", 100, 50))).toBe(true);
  });
});
