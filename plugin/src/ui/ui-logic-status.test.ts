import { describe, expect, test } from "bun:test";
import { CLOSE_FOR_UPDATE_TOAST, isInboundMessage } from "../protocol";
import {
  DAEMON_OLD_LABEL,
  daemonMessageAction,
  healthAfterClose,
  INITIAL_LINK_HEALTH,
  isUiOnlyControlType,
  mainMessageAction,
  NO_CHALLENGE_LIMIT,
  NOT_RUNNING_LABEL,
  reloadDisabled,
  staleBanner,
  staleWarning,
  statusOverride,
  VERIFY_FAILED_LABEL,
} from "./ui-logic";

describe("version banner", () => {
  test("daemon newer: updated text and a Reload button", () => {
    expect(staleBanner("0.4.0", "0.5.0")).toEqual({
      text: "turbofig was updated to 0.5.0.",
      canReload: true,
    });
  });
  test("compares numbers, not strings (0.10.0 is newer than 0.9.0)", () => {
    expect(staleBanner("0.9.0", "0.10.0")?.canReload).toBe(true);
  });
  test("plugin newer: update text and no Reload button", () => {
    expect(staleBanner("0.5.0", "0.4.0")).toEqual({
      text: "turbofig 0.4.0 is older than this plugin. Update turbofig.",
      canReload: false,
    });
  });
  test("same or unknown versions give no banner", () => {
    expect(staleBanner("0.5.0", "0.5.0")).toBeNull();
    expect(staleBanner("0.5.0", "")).toBeNull();
    expect(staleWarning("0.5.0", "0.5.0")).toBe("");
  });
  test("banner texts stay short for the 300 px panel", () => {
    expect((staleBanner("0.4.0", "0.5.0")?.text ?? "").length).toBeLessThan(60);
    expect((staleBanner("0.5.0", "0.4.0")?.text ?? "").length).toBeLessThan(70);
  });
});

describe("Reload button", () => {
  test("disabled while a job runs, enabled when idle", () => {
    expect(reloadDisabled(1)).toBe(true);
    expect(reloadDisabled(3)).toBe(true);
    expect(reloadDisabled(0)).toBe(false);
  });
  test("the close message is accepted by the main thread", () => {
    expect(isInboundMessage({ type: "CLOSE_FOR_UPDATE" })).toBe(true);
    expect(CLOSE_FOR_UPDATE_TOAST).toBe("turbofig was updated. Press ⌥⌘P to reopen it.");
  });
  test("the close message is dropped from the daemon relay and the main-thread relay", () => {
    expect(daemonMessageAction("CLOSE_FOR_UPDATE")).toBe("drop");
    expect(isUiOnlyControlType("CLOSE_FOR_UPDATE")).toBe(true);
    expect(mainMessageAction("CLOSE_FOR_UPDATE")).toBe("drop");
  });
  test("other daemon types still relay", () => {
    expect(daemonMessageAction("EXECUTE")).toBe("relay");
    expect(daemonMessageAction("WELCOME")).toBe("welcome");
  });
});

describe("status labels", () => {
  test("token reset label says what to do", () => {
    expect(VERIFY_FAILED_LABEL).toBe(
      "turbofig was reset, or another turbofig uses its port. Close and reopen this plugin.",
    );
    expect(VERIFY_FAILED_LABEL.length).toBeLessThanOrEqual(100);
    expect(statusOverride({ misses: 0, failed: true })).toBe(VERIFY_FAILED_LABEL);
  });
  test("port taken or old daemon label covers both causes", () => {
    expect(DAEMON_OLD_LABEL).toContain("Another app may be using turbofig's port");
    expect(DAEMON_OLD_LABEL).toContain("older than this plugin");
    expect(statusOverride({ misses: NO_CHALLENGE_LIMIT, failed: false })).toBe(DAEMON_OLD_LABEL);
  });
  test("not running label names the app and Terminal", () => {
    expect(NOT_RUNNING_LABEL).toBe(
      "turbofig is not running. Open the turbofig app, or run turbofig in Terminal.",
    );
  });
  test("labels fit two lines of the 300 px panel (under 90 chars)", () => {
    for (const l of [VERIFY_FAILED_LABEL, DAEMON_OLD_LABEL, NOT_RUNNING_LABEL]) {
      expect(l.length).toBeLessThan(90);
    }
  });
});

describe("not running beats a stale verify failure", () => {
  const failed = { misses: 0, failed: true };
  test("a socket that never opens clears the failure", () => {
    const h = healthAfterClose(failed, {
      opened: false,
      sawChallenge: false,
      failedVerification: false,
    });
    expect(h.failed).toBe(false);
    expect(statusOverride(h)).toBeNull();
  });
  test("a socket that opened and failed again keeps the failure", () => {
    const h = healthAfterClose(failed, {
      opened: true,
      sawChallenge: true,
      failedVerification: true,
    });
    expect(statusOverride(h)).toBe(VERIFY_FAILED_LABEL);
  });
  test("a fresh failure still sticks", () => {
    const h = healthAfterClose(INITIAL_LINK_HEALTH, {
      opened: true,
      sawChallenge: true,
      failedVerification: true,
    });
    expect(h.failed).toBe(true);
  });
});
