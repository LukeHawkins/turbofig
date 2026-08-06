/**
 * Tests for the `impeccable` taste profile.
 *
 * The profile is a plain JS script. This test evaluates it via the Function
 * constructor to extract `taste`, then asserts the full schema contract.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const src = readFileSync(join(import.meta.dir, "../../skills/profiles/impeccable.js"), "utf8");
const taste = new Function(`${src}\nreturn taste;`)() as Record<string, unknown>;

/** Core-floor blocklist ids that must always be present. */
const CORE_FLOOR_IDS = [
  "centered-everything",
  "generic-gradient",
  "emoji-bullets",
  "dead-whitespace",
  "gray-1px-borders-everywhere",
  "pure-black",
  "three-card-row",
];

describe("impeccable taste profile", () => {
  test("id equals 'impeccable'", () => {
    expect(taste.id).toBe("impeccable");
  });

  test("label is a non-empty string", () => {
    expect(typeof taste.label).toBe("string");
    expect((taste.label as string).length).toBeGreaterThan(0);
  });

  test("spacing is a non-empty ascending array of numbers", () => {
    const spacing = taste.spacing as unknown[];
    expect(Array.isArray(spacing)).toBe(true);
    expect(spacing.length).toBeGreaterThan(0);
    for (const v of spacing) {
      expect(typeof v).toBe("number");
    }
    for (let i = 1; i < spacing.length; i++) {
      expect(spacing[i] as number).toBeGreaterThan(spacing[i - 1] as number);
    }
  });

  test("type has correct numeric keys and non-empty arrays", () => {
    const t = taste.type as Record<string, unknown>;
    expect(typeof t).toBe("object");
    expect(t).not.toBeNull();
    expect(typeof t.baseSize).toBe("number");
    expect(typeof t.ratio).toBe("number");
    expect(Array.isArray(t.scale)).toBe(true);
    expect((t.scale as unknown[]).length).toBeGreaterThan(0);
    expect(Array.isArray(t.displayFamilies)).toBe(true);
    expect((t.displayFamilies as unknown[]).length).toBeGreaterThan(0);
    expect(Array.isArray(t.textFamilies)).toBe(true);
    expect((t.textFamilies as unknown[]).length).toBeGreaterThan(0);
  });

  test("grid has correct numeric keys", () => {
    const g = taste.grid as Record<string, unknown>;
    expect(typeof g).toBe("object");
    expect(g).not.toBeNull();
    expect(typeof g.columns).toBe("number");
    expect(typeof g.gutter).toBe("number");
    expect(typeof g.margin).toBe("number");
  });

  test("contrast meets WCAG AA minimums", () => {
    const c = taste.contrast as Record<string, unknown>;
    expect(typeof c).toBe("object");
    expect(c).not.toBeNull();
    expect(typeof c.bodyMin).toBe("number");
    expect(typeof c.largeMin).toBe("number");
    expect(c.bodyMin as number).toBeGreaterThanOrEqual(4.5);
    expect(c.largeMin as number).toBeGreaterThanOrEqual(3);
  });

  test("hierarchy is a non-empty string array", () => {
    const h = taste.hierarchy as unknown[];
    expect(Array.isArray(h)).toBe(true);
    expect(h.length).toBeGreaterThan(0);
    for (const v of h) {
      expect(typeof v).toBe("string");
    }
  });

  test("restraint is a non-empty string array", () => {
    const r = taste.restraint as unknown[];
    expect(Array.isArray(r)).toBe(true);
    expect(r.length).toBeGreaterThan(0);
    for (const v of r) {
      expect(typeof v).toBe("string");
    }
  });

  test("blocklist is a non-empty string array", () => {
    const bl = taste.blocklist as unknown[];
    expect(Array.isArray(bl)).toBe(true);
    expect(bl.length).toBeGreaterThan(0);
    for (const v of bl) {
      expect(typeof v).toBe("string");
    }
  });

  test("blocklist includes all seven core-floor ids", () => {
    const bl = taste.blocklist as string[];
    for (const id of CORE_FLOOR_IDS) {
      expect(bl).toContain(id);
    }
  });
});
