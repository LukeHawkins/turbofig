/**
 * Tests for the `editorial` and `minimal` taste profiles.
 *
 * Each profile is a plain JS script. This test evaluates each via the Function
 * constructor to extract `taste`, then asserts the full schema contract. It
 * also asserts that the two profiles are genuinely distinct from each other.
 */

import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const editorialSrc = readFileSync(
  join(import.meta.dir, "../../skills/profiles/editorial.js"),
  "utf8",
);
const minimalSrc = readFileSync(join(import.meta.dir, "../../skills/profiles/minimal.js"), "utf8");

const editorial = new Function(`${editorialSrc}\nreturn taste;`)() as Record<string, unknown>;
const minimal = new Function(`${minimalSrc}\nreturn taste;`)() as Record<string, unknown>;

/** Core-floor blocklist ids that must always be present in every profile. */
const CORE_FLOOR_IDS = [
  "centered-everything",
  "generic-gradient",
  "emoji-bullets",
  "dead-whitespace",
  "gray-1px-borders-everywhere",
  "pure-black",
  "three-card-row",
];

/** Assert the full schema contract for one profile. */
function assertSchema(taste: Record<string, unknown>, expectedId: string): void {
  expect(taste.id).toBe(expectedId);

  expect(typeof taste.label).toBe("string");
  expect((taste.label as string).length).toBeGreaterThan(0);

  const spacing = taste.spacing as unknown[];
  expect(Array.isArray(spacing)).toBe(true);
  expect(spacing.length).toBeGreaterThan(0);
  for (const v of spacing) {
    expect(typeof v).toBe("number");
  }
  for (let i = 1; i < spacing.length; i++) {
    expect(spacing[i] as number).toBeGreaterThan(spacing[i - 1] as number);
  }

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

  const g = taste.grid as Record<string, unknown>;
  expect(typeof g).toBe("object");
  expect(g).not.toBeNull();
  expect(typeof g.columns).toBe("number");
  expect(typeof g.gutter).toBe("number");
  expect(typeof g.margin).toBe("number");

  const c = taste.contrast as Record<string, unknown>;
  expect(typeof c).toBe("object");
  expect(c).not.toBeNull();
  expect(typeof c.bodyMin).toBe("number");
  expect(typeof c.largeMin).toBe("number");
  expect(c.bodyMin as number).toBeGreaterThanOrEqual(4.5);
  expect(c.largeMin as number).toBeGreaterThanOrEqual(3);

  const h = taste.hierarchy as unknown[];
  expect(Array.isArray(h)).toBe(true);
  expect(h.length).toBeGreaterThan(0);
  for (const v of h) {
    expect(typeof v).toBe("string");
  }

  const r = taste.restraint as unknown[];
  expect(Array.isArray(r)).toBe(true);
  expect(r.length).toBeGreaterThan(0);
  for (const v of r) {
    expect(typeof v).toBe("string");
  }

  const bl = taste.blocklist as unknown[];
  expect(Array.isArray(bl)).toBe(true);
  expect(bl.length).toBeGreaterThan(0);
  for (const v of bl) {
    expect(typeof v).toBe("string");
  }
}

describe("editorial taste profile", () => {
  test("id equals 'editorial'", () => {
    expect(editorial.id).toBe("editorial");
  });

  test("satisfies full schema contract", () => {
    assertSchema(editorial, "editorial");
  });

  test("blocklist includes all seven core-floor ids", () => {
    const bl = editorial.blocklist as string[];
    for (const id of CORE_FLOOR_IDS) {
      expect(bl).toContain(id);
    }
  });
});

describe("minimal taste profile", () => {
  test("id equals 'minimal'", () => {
    expect(minimal.id).toBe("minimal");
  });

  test("satisfies full schema contract", () => {
    assertSchema(minimal, "minimal");
  });

  test("blocklist includes all seven core-floor ids", () => {
    const bl = minimal.blocklist as string[];
    for (const id of CORE_FLOOR_IDS) {
      expect(bl).toContain(id);
    }
  });
});

describe("editorial vs minimal distinctness", () => {
  test("spacing arrays are not equal between editorial and minimal", () => {
    expect(JSON.stringify(editorial.spacing)).not.toBe(JSON.stringify(minimal.spacing));
  });

  test("type.scale arrays are not equal between editorial and minimal", () => {
    const eScale = (editorial.type as Record<string, unknown>).scale;
    const mScale = (minimal.type as Record<string, unknown>).scale;
    expect(JSON.stringify(eScale)).not.toBe(JSON.stringify(mScale));
  });

  test("grid is not equal between editorial and minimal", () => {
    expect(JSON.stringify(editorial.grid)).not.toBe(JSON.stringify(minimal.grid));
  });

  test("at least two of spacing, type.scale, and grid differ", () => {
    const spacingDiffers = JSON.stringify(editorial.spacing) !== JSON.stringify(minimal.spacing);
    const scaleDiffers =
      JSON.stringify((editorial.type as Record<string, unknown>).scale) !==
      JSON.stringify((minimal.type as Record<string, unknown>).scale);
    const gridDiffers = JSON.stringify(editorial.grid) !== JSON.stringify(minimal.grid);

    const diffCount = [spacingDiffers, scaleDiffers, gridDiffers].filter(Boolean).length;
    expect(diffCount).toBeGreaterThanOrEqual(2);
  });
});
