import { describe, expect, test } from "bun:test";
import {
  connStateFromEvent,
  formatFileLine,
  formatPairing,
  formatSession,
  KNOWN_PROFILES,
  profileToSelectValue,
} from "./ui-logic";

describe("connStateFromEvent", () => {
  test("open event returns connected state", () => {
    const r = connStateFromEvent("open", 0);
    expect(r.state).toBe("connected");
    expect(r.label).toBe("Connected");
  });

  test("open event returns connected regardless of attempt count", () => {
    const r = connStateFromEvent("open", 10);
    expect(r.state).toBe("connected");
  });

  test("attempt at 0 returns connecting state", () => {
    const r = connStateFromEvent("attempt", 0);
    expect(r.state).toBe("connecting");
  });

  test("attempt at 1 returns reconnecting state", () => {
    const r = connStateFromEvent("attempt", 1);
    expect(r.state).toBe("reconnecting");
  });

  test("attempt at 4 returns reconnecting state", () => {
    const r = connStateFromEvent("attempt", 4);
    expect(r.state).toBe("reconnecting");
  });

  test("attempt at 5 returns offline state", () => {
    const r = connStateFromEvent("attempt", 5);
    expect(r.state).toBe("offline");
    expect(r.label).toBe("Offline");
  });

  test("attempt at 10 returns offline state", () => {
    const r = connStateFromEvent("attempt", 10);
    expect(r.state).toBe("offline");
  });

  test("close at 0 returns connecting state", () => {
    const r = connStateFromEvent("close", 0);
    expect(r.state).toBe("connecting");
  });

  test("close at 3 returns reconnecting state", () => {
    const r = connStateFromEvent("close", 3);
    expect(r.state).toBe("reconnecting");
  });

  test("close at 5 returns offline state", () => {
    const r = connStateFromEvent("close", 5);
    expect(r.state).toBe("offline");
  });

  test("error event returns reconnecting state", () => {
    const r = connStateFromEvent("error", 0);
    expect(r.state).toBe("reconnecting");
  });

  test("error event with high attempt count still returns reconnecting", () => {
    const r = connStateFromEvent("error", 10);
    expect(r.state).toBe("reconnecting");
  });

  test("all events return a non-empty label", () => {
    const cases: Array<[Parameters<typeof connStateFromEvent>[0], number]> = [
      ["open", 0],
      ["attempt", 0],
      ["attempt", 3],
      ["attempt", 5],
      ["close", 1],
      ["error", 0],
    ];
    for (const [event, attempt] of cases) {
      const r = connStateFromEvent(event, attempt);
      expect(r.label.length).toBeGreaterThan(0);
    }
  });
});

describe("formatFileLine", () => {
  test("returns name with shortened key for full inputs", () => {
    const result = formatFileLine("ABCDEF1234567890", "My Design");
    expect(result).toBe("My Design (ABCDEF12)");
  });

  test("returns truncated key when name is empty", () => {
    const result = formatFileLine("ABCDEF12EXTRA", "");
    expect(result).toBe("ABCDEF12");
  });

  test("returns just the name when fileKey is empty", () => {
    const result = formatFileLine("", "My Design");
    expect(result).toBe("My Design");
  });

  test("returns No file when both are empty", () => {
    const result = formatFileLine("", "");
    expect(result).toBe("No file");
  });

  test("handles a short key without truncation", () => {
    const result = formatFileLine("ABC", "File");
    expect(result).toBe("File (ABC)");
  });

  test("key exactly 8 characters is not truncated", () => {
    const result = formatFileLine("12345678", "Doc");
    expect(result).toBe("Doc (12345678)");
  });
});

describe("formatSession", () => {
  test("returns No active session when empty", () => {
    expect(formatSession("")).toBe("No active session");
  });

  test("returns short id unchanged", () => {
    expect(formatSession("abc123")).toBe("abc123");
  });

  test("returns id unchanged at exactly 12 characters", () => {
    expect(formatSession("123456789012")).toBe("123456789012");
  });

  test("truncates id at 13 characters with ...", () => {
    expect(formatSession("1234567890123")).toBe("123456789012...");
  });

  test("truncates long id with ...", () => {
    expect(formatSession("abcdefghijklmnopq")).toBe("abcdefghijkl...");
  });
});

describe("profileToSelectValue", () => {
  test("known profiles return their id as value with isCustom false", () => {
    for (const id of KNOWN_PROFILES) {
      const r = profileToSelectValue(id);
      expect(r.value).toBe(id);
      expect(r.isCustom).toBe(false);
    }
  });

  test("unknown profile id returns custom with isCustom true", () => {
    const r = profileToSelectValue("my-brand");
    expect(r.value).toBe("custom");
    expect(r.isCustom).toBe(true);
  });

  test("empty string is not a known id and returns custom", () => {
    const r = profileToSelectValue("");
    expect(r.value).toBe("custom");
    expect(r.isCustom).toBe(true);
  });

  test("impeccable is a known profile", () => {
    const r = profileToSelectValue("impeccable");
    expect(r.isCustom).toBe(false);
  });

  test("minimal is a known profile", () => {
    const r = profileToSelectValue("minimal");
    expect(r.isCustom).toBe(false);
  });
});

describe("formatPairing", () => {
  test("empty sessionId returns paired false", () => {
    const r = formatPairing("", "My File");
    expect(r.paired).toBe(false);
  });

  test("empty sessionId label says not paired", () => {
    const r = formatPairing("", "My File");
    expect(r.label).toContain("Not paired");
  });

  test("empty sessionId with empty fileName returns paired false", () => {
    const r = formatPairing("", "");
    expect(r.paired).toBe(false);
    expect(r.label).toContain("Not paired");
  });

  test("non-empty sessionId returns paired true", () => {
    const r = formatPairing("ses-abc", "My File");
    expect(r.paired).toBe(true);
  });

  test("short sessionId appears in label without truncation", () => {
    const r = formatPairing("ses-abc", "My File");
    expect(r.label).toContain("ses-abc");
  });

  test("sessionId of exactly 12 characters is not truncated", () => {
    const r = formatPairing("123456789012", "Doc");
    expect(r.label).toContain("123456789012");
    expect(r.label).not.toContain("...");
  });

  test("sessionId longer than 12 characters is truncated with ...", () => {
    const r = formatPairing("1234567890123", "Doc");
    expect(r.label).toContain("123456789012...");
  });

  test("non-empty fileName is included in the paired label", () => {
    const r = formatPairing("ses-abc", "Brand System");
    expect(r.label).toContain("Brand System");
  });

  test("empty fileName does not cause a broken label", () => {
    const r = formatPairing("ses-abc", "");
    expect(r.paired).toBe(true);
    expect(r.label.length).toBeGreaterThan(0);
    expect(r.label).not.toContain("undefined");
    expect(r.label).not.toContain("null");
  });

  test("long sessionId with fileName includes both truncated id and file name", () => {
    const r = formatPairing("abcdefghijklmnop", "My Design");
    expect(r.label).toContain("abcdefghijkl...");
    expect(r.label).toContain("My Design");
  });
});
