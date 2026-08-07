import { describe, expect, test } from "bun:test";
import {
  applySetProfile,
  backoffDelayMs,
  buildExecuteError,
  buildExecuteSuccess,
  buildFileInfo,
  buildResult,
  buildScreenshot,
  buildSelection,
  buildSetProfile,
  DEPRECATION_PREAMBLE,
  isDaemonMessage,
  isInboundMessage,
  MAX_SELECTION_DEPTH,
  readProfileId,
  safeResult,
  serializeNode,
  toSelectionItem,
  wrapUserCode,
} from "./protocol";

describe("isDaemonMessage", () => {
  test("accepts a valid FILE_INFO message", () => {
    expect(
      isDaemonMessage({ type: "FILE_INFO", fileKey: "abc123", name: "My File", profileId: "" }),
    ).toBe(true);
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

  test("fields: ['children'] at depth 0 does not add a children key (reserved)", () => {
    // children is owned by the depth mechanism; passing it in fields must not bypass the cap.
    const child = { id: "1:2", name: "Child", type: "TEXT", x: 0, y: 0, width: 10, height: 10 };
    const node = { ...baseNode, children: [child] };
    const result = serializeNode(node, ["children"], 0);
    expect("children" in result).toBe(false);
  });

  test("a plain JSON-safe object field is copied when requested", () => {
    // A plain fills array is JSON-safe and must be included when listed in fields.
    const fills = [{ type: "SOLID", color: { r: 1, g: 0, b: 0 } }];
    const node = { ...baseNode, fills };
    const result = serializeNode(node, ["fills"], 0);
    expect(result.fills).toEqual(fills);
  });

  test("a non-JSON-safe field is skipped and serializeNode does not throw", () => {
    // A circular reference is not JSON-safe; the field must be silently dropped.
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    const node = { ...baseNode, proxy: circular };
    expect(() => serializeNode(node, ["proxy"], 0)).not.toThrow();
    const result = serializeNode(node, ["proxy"], 0);
    expect("proxy" in result).toBe(false);
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

describe("buildFileInfo", () => {
  test("returns a FILE_INFO carrying fileKey, name, and profileId", () => {
    const msg = buildFileInfo("key1", "Design File", "minimal");
    expect(msg.type).toBe("FILE_INFO");
    expect(msg.fileKey).toBe("key1");
    expect(msg.name).toBe("Design File");
    expect(msg.profileId).toBe("minimal");
  });

  test("result passes the isDaemonMessage guard", () => {
    expect(isDaemonMessage(buildFileInfo("k", "n", ""))).toBe(true);
  });

  test("accepts an empty profileId string", () => {
    const msg = buildFileInfo("k", "n", "");
    expect(msg.profileId).toBe("");
  });
});

describe("isDaemonMessage: SET_PROFILE branch", () => {
  test("accepts a well-formed SET_PROFILE message", () => {
    expect(isDaemonMessage({ type: "SET_PROFILE", profileId: "minimal" })).toBe(true);
  });

  test("accepts a SET_PROFILE with an empty profileId string", () => {
    expect(isDaemonMessage({ type: "SET_PROFILE", profileId: "" })).toBe(true);
  });

  test("rejects SET_PROFILE missing profileId", () => {
    expect(isDaemonMessage({ type: "SET_PROFILE" })).toBe(false);
  });

  test("rejects SET_PROFILE with a numeric profileId", () => {
    expect(isDaemonMessage({ type: "SET_PROFILE", profileId: 42 })).toBe(false);
  });

  test("FILE_INFO with profileId passes the guard", () => {
    expect(
      isDaemonMessage({ type: "FILE_INFO", fileKey: "f", name: "n", profileId: "minimal" }),
    ).toBe(true);
  });

  test("FILE_INFO missing profileId is rejected", () => {
    expect(isDaemonMessage({ type: "FILE_INFO", fileKey: "f", name: "n" })).toBe(false);
  });
});

describe("profile store helpers", () => {
  /** Builds an in-memory PluginDataStore for testing. */
  function makeMockStore(): {
    getPluginData(key: string): string;
    setPluginData(key: string, value: string): void;
  } {
    const data: Record<string, string> = {};
    return {
      getPluginData(key: string): string {
        return key in data ? data[key] : "";
      },
      setPluginData(key: string, value: string): void {
        data[key] = value;
      },
    };
  }

  test("readProfileId returns empty string when no profile is set", () => {
    const store = makeMockStore();
    expect(readProfileId(store)).toBe("");
  });

  test("applySetProfile writes a profileId that readProfileId then returns", () => {
    const store = makeMockStore();
    applySetProfile(store, "minimal");
    expect(readProfileId(store)).toBe("minimal");
  });

  test("buildFileInfo round-trip: SET_PROFILE -> setPluginData -> FILE_INFO carries profileId", () => {
    // Simulate the SET_PROFILE -> applySetProfile -> emitFileInfo path.
    const store = makeMockStore();
    applySetProfile(store, "minimal");
    const msg = buildFileInfo("k", "n", readProfileId(store));
    expect(msg.profileId).toBe("minimal");
    expect(isDaemonMessage(msg)).toBe(true);
  });

  test("overwriting a profile replaces the previous value", () => {
    const store = makeMockStore();
    applySetProfile(store, "minimal");
    applySetProfile(store, "vibrant");
    expect(readProfileId(store)).toBe("vibrant");
  });
});

describe("buildSetProfile", () => {
  /** Minimal in-memory PluginDataStore for testing. */
  function makeMockStore(): {
    getPluginData(key: string): string;
    setPluginData(key: string, value: string): void;
  } {
    const data: Record<string, string> = {};
    return {
      getPluginData(key: string): string {
        return key in data ? data[key] : "";
      },
      setPluginData(key: string, value: string): void {
        data[key] = value;
      },
    };
  }

  // One parameterised test covers all known profile ids including a custom one.
  for (const id of ["impeccable", "editorial", "minimal", "none", "my-brand"]) {
    test(`returns { type: "SET_PROFILE", profileId: "${id}" } and passes isDaemonMessage`, () => {
      const msg = buildSetProfile(id);
      expect(msg).toEqual({ type: "SET_PROFILE", profileId: id });
      expect(isDaemonMessage(msg)).toBe(true);
    });
  }

  test("applySetProfile does not write when profileId is an empty string", () => {
    const store = makeMockStore();
    applySetProfile(store, "");
    expect(readProfileId(store)).toBe("");
  });

  test("value-flow: buildSetProfile -> applySetProfile -> readProfileId -> buildFileInfo carries profileId", () => {
    const store = makeMockStore();
    const profileId = buildSetProfile("minimal").profileId;
    applySetProfile(store, profileId);
    expect(readProfileId(store)).toBe("minimal");
    const fileInfo = buildFileInfo("k", "n", readProfileId(store));
    expect(fileInfo.profileId).toBe("minimal");
    expect(isDaemonMessage(fileInfo)).toBe(true);
  });
});

describe("isDaemonMessage: WELCOME branch", () => {
  test("accepts a valid WELCOME message", () => {
    expect(isDaemonMessage({ type: "WELCOME", version: "0.5.0" })).toBe(true);
  });

  test("accepts WELCOME with an empty version string", () => {
    expect(isDaemonMessage({ type: "WELCOME", version: "" })).toBe(true);
  });

  test("rejects WELCOME missing version", () => {
    expect(isDaemonMessage({ type: "WELCOME" })).toBe(false);
  });

  test("rejects WELCOME with a numeric version", () => {
    expect(isDaemonMessage({ type: "WELCOME", version: 1 })).toBe(false);
  });

  test("accepts WELCOME with mcpPort present (new daemon)", () => {
    expect(isDaemonMessage({ type: "WELCOME", version: "0.11.0", mcpPort: 18846 })).toBe(true);
  });

  test("accepts WELCOME without mcpPort (older daemon, back-compat)", () => {
    expect(isDaemonMessage({ type: "WELCOME", version: "0.10.0" })).toBe(true);
  });
});

describe("isDaemonMessage: sessionId on request messages", () => {
  test("STATUS with sessionId passes the guard", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: 1, sessionId: "ses-abc" })).toBe(true);
  });

  test("STATUS with empty sessionId passes the guard", () => {
    expect(isDaemonMessage({ type: "STATUS", requestId: 1, sessionId: "" })).toBe(true);
  });

  test("EXECUTE with sessionId passes the guard", () => {
    expect(
      isDaemonMessage({ type: "EXECUTE", requestId: 2, code: "return 1;", sessionId: "ses-xyz" }),
    ).toBe(true);
  });

  test("GET_SELECTION with sessionId passes the guard", () => {
    expect(isDaemonMessage({ type: "GET_SELECTION", requestId: 3, sessionId: "ses-abc" })).toBe(
      true,
    );
  });

  test("SCREENSHOT with sessionId passes the guard", () => {
    expect(isDaemonMessage({ type: "SCREENSHOT", requestId: 4, sessionId: "ses-abc" })).toBe(true);
  });
});

describe("isInboundMessage", () => {
  test("accepts STATUS", () => {
    expect(isInboundMessage({ type: "STATUS", requestId: 1 })).toBe(true);
  });

  test("accepts EXECUTE", () => {
    expect(isInboundMessage({ type: "EXECUTE", requestId: 1, code: "return 1;" })).toBe(true);
  });

  test("accepts GET_SELECTION", () => {
    expect(isInboundMessage({ type: "GET_SELECTION", requestId: 2 })).toBe(true);
  });

  test("accepts SCREENSHOT", () => {
    expect(isInboundMessage({ type: "SCREENSHOT", requestId: 3 })).toBe(true);
  });

  test("accepts SET_PROFILE", () => {
    expect(isInboundMessage({ type: "SET_PROFILE", profileId: "minimal" })).toBe(true);
  });

  test("rejects FILE_INFO (outbound only)", () => {
    expect(isInboundMessage({ type: "FILE_INFO", fileKey: "k", name: "n", profileId: "" })).toBe(
      false,
    );
  });

  test("rejects RESULT (outbound only)", () => {
    expect(isInboundMessage({ type: "RESULT", requestId: 1 })).toBe(false);
  });

  test("rejects null", () => {
    expect(isInboundMessage(null)).toBe(false);
  });

  test("rejects an unknown type", () => {
    expect(isInboundMessage({ type: "UNKNOWN" })).toBe(false);
  });

  test("rejects EXECUTE missing code", () => {
    expect(isInboundMessage({ type: "EXECUTE", requestId: 1 })).toBe(false);
  });

  test("accepts SET_PORT with a numeric port", () => {
    expect(isInboundMessage({ type: "SET_PORT", port: 18847 })).toBe(true);
  });

  test("rejects SET_PORT missing port", () => {
    expect(isInboundMessage({ type: "SET_PORT" })).toBe(false);
  });

  test("rejects SET_PORT with a string port", () => {
    expect(isInboundMessage({ type: "SET_PORT", port: "18847" })).toBe(false);
  });
});

describe("PortMessage shape", () => {
  // PortMessage is a main-thread-to-UI message only and has no guard function.
  // These tests verify the shape is structurally correct as a plain object.
  test("PORT message has type and numeric port", () => {
    const msg = { type: "PORT" as const, port: 8080 };
    expect(msg.type).toBe("PORT");
    expect(typeof msg.port).toBe("number");
  });

  test("PORT message carries the default port 18847", () => {
    const msg = { type: "PORT" as const, port: 18847 };
    expect(msg.port).toBe(18847);
  });
});

describe("ResizeMessage", () => {
  test("isInboundMessage accepts a well-formed RESIZE message", () => {
    expect(isInboundMessage({ type: "RESIZE", width: 300, height: 200 })).toBe(true);
  });

  test("isInboundMessage accepts the advanced-screen dimensions", () => {
    expect(isInboundMessage({ type: "RESIZE", width: 300, height: 440 })).toBe(true);
  });

  test("isInboundMessage rejects RESIZE with a non-numeric width", () => {
    expect(isInboundMessage({ type: "RESIZE", width: "300", height: 200 })).toBe(false);
  });

  test("isInboundMessage rejects RESIZE with a non-numeric height", () => {
    expect(isInboundMessage({ type: "RESIZE", width: 300, height: "200" })).toBe(false);
  });

  test("isInboundMessage rejects RESIZE missing both dimensions", () => {
    expect(isInboundMessage({ type: "RESIZE" })).toBe(false);
  });

  test("RESIZE round-trips as a plain object", () => {
    const msg = { type: "RESIZE" as const, width: 300, height: 200 };
    expect(msg.type).toBe("RESIZE");
    expect(msg.width).toBe(300);
    expect(msg.height).toBe(200);
    expect(isInboundMessage(msg)).toBe(true);
  });
});
