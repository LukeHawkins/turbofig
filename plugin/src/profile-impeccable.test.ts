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

/** Relative luminance of a #rrggbb hex colour per WCAG 2.1. */
function luminance(hex: string): number {
  const h = hex.replace("#", "");
  const chan = [0, 2, 4].map((i) => {
    const c = Number.parseInt(h.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * chan[0] + 0.7152 * chan[1] + 0.0722 * chan[2];
}

/** WCAG contrast ratio between two #rrggbb hex colours. */
function contrastRatio(a: string, b: string): number {
  const la = luminance(a);
  const lb = luminance(b);
  const hi = Math.max(la, lb);
  const lo = Math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

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

  test("palette exists and carries all six required keys", () => {
    const p = taste.palette as Record<string, unknown>;
    expect(typeof p).toBe("object");
    expect(p).not.toBeNull();

    const requiredKeys = ["brand", "accent", "surface", "surfaceAlt", "text", "textMuted"];
    for (const key of requiredKeys) {
      expect(typeof p[key]).toBe("string");
      expect((p[key] as string).length).toBeGreaterThan(0);
    }
  });

  test("palette.text is not pure black (#000000)", () => {
    // The pure-black blocklist entry in this profile forbids #000000 fills.
    // text is the primary text token and must obey this constraint.
    const p = taste.palette as Record<string, string>;
    expect(p.text).not.toBe("#000000");
  });

  test("palette.brand is not a purple or violet hue", () => {
    // The live slop incident produced an all-purple design because the workflow
    // defaulted to a generic purple when no palette was present. This deny-list
    // guards the impeccable brand token against that regression. The listed
    // values cover the most common CSS / Tailwind / Material purple/violet hex
    // values. The check is case-insensitive to handle both upper and lower case.
    const PURPLE_DENY_LIST = new Set([
      "#6b21a8",
      "#7c3aed",
      "#8b5cf6",
      "#9333ea",
      "#a855f7",
      "#7b2fbe",
      "#5b21b6",
      "#4c1d95",
      "#6d28d9",
      "#7e22ce",
      "#9b59b6",
      "#8e44ad",
      "#6c3483",
      "#4a235a",
      "#512da8",
      "#673ab7",
      "#9c27b0",
      "#7b1fa2",
      "#6a1b9a",
      "#4a148c",
    ]);
    const p = taste.palette as Record<string, string>;
    expect(PURPLE_DENY_LIST.has(p.brand.toLowerCase())).toBe(false);
  });

  test("text and textMuted meet AA (>= 4.5:1) on surface", () => {
    // Both are used as body copy on the surface, so both must clear the
    // 4.5:1 AA body minimum. This guards future palette edits.
    const p = taste.palette as Record<string, string>;
    expect(contrastRatio(p.text, p.surface)).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(p.textMuted, p.surface)).toBeGreaterThanOrEqual(4.5);
  });
});
