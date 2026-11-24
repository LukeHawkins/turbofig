import { describe, expect, test } from "bun:test";
import {
  ADV_HINT_HEIGHT,
  isUiOnlyControlType,
  mainMessageAction,
  screenSize,
  welcomeHasValidMode,
} from "./ui-logic";

describe("connection relay filter and readiness", () => {
  test("mainMessageAction drops UI-only control types so an EXECUTE job cannot send them", () => {
    for (const type of ["SET_CONNECTION_MODE", "AUTH", "CONNECTION_MODE", "WELCOME", "CHALLENGE"]) {
      expect(mainMessageAction(type)).toBe("drop");
      expect(isUiOnlyControlType(type)).toBe(true);
    }
    expect(mainMessageAction("RESULT")).toBe("relay");
    expect(mainMessageAction("PORT")).toBe("port");
    expect(isUiOnlyControlType("RESULT")).toBe(false);
  });

  test("welcomeHasValidMode needs a WELCOME with a valid connectionMode", () => {
    expect(welcomeHasValidMode({ type: "WELCOME", connectionMode: "file" })).toBe(true);
    expect(welcomeHasValidMode({ type: "WELCOME", connectionMode: "mcp" })).toBe(true);
    expect(welcomeHasValidMode({ type: "WELCOME" })).toBe(false);
    expect(welcomeHasValidMode({ type: "WELCOME", connectionMode: "bogus" })).toBe(false);
    expect(welcomeHasValidMode({ type: "CONNECTION_MODE", mode: "mcp" })).toBe(false);
    expect(welcomeHasValidMode(null)).toBe(false);
  });

  test("the advanced panel is shorter by the hint height when the daemon is ready", () => {
    for (const showLog of [true, false]) {
      const hint = screenSize("advanced", showLog, true).height;
      const ready = screenSize("advanced", showLog, false).height;
      expect(hint - ready).toBe(ADV_HINT_HEIGHT);
    }
  });
});
