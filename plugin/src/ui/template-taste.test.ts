/**
 * Taste-compliance tests for template.html.
 * Guards the impeccable anti-slop rules and type-scale membership.
 */
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

/** Read the raw template text. The __UI_BUNDLE__ token is not expanded here. */
const template = readFileSync(join(import.meta.dir, "template.html"), "utf-8");

/** Font sizes from the impeccable type scale (px). */
const VALID_TYPE_SCALE = new Set([12, 14, 16, 20, 25, 31, 39, 49, 61]);

describe("template.html — impeccable taste compliance", () => {
  test("declares an Inter font-family", () => {
    expect(template).toContain("Inter");
  });

  test("contains no linear-gradient", () => {
    expect(template).not.toContain("linear-gradient");
  });

  test("contains no radial-gradient", () => {
    expect(template).not.toContain("radial-gradient");
  });

  test("contains no pure-black 3-digit hex (#000 not followed by hex digit)", () => {
    // Matches #000 followed by a non-hex word boundary. Skips #000abc, #0001ff etc.
    expect(template).not.toMatch(/#000\b/);
  });

  test("contains no pure-black 6-digit hex (#000000)", () => {
    expect(template).not.toContain("#000000");
  });

  test("contains no rgb(0, 0, 0) pure-black value", () => {
    expect(template).not.toMatch(/rgb\(\s*0\s*,\s*0\s*,\s*0\s*\)/);
  });

  test("contains no bare 'black' colour value", () => {
    // Matches ': black' or ':black' as a CSS value start.
    expect(template).not.toMatch(/:\s*black\b/);
  });

  test("contains no text-align: center", () => {
    expect(template).not.toMatch(/text-align\s*:\s*center/);
  });

  test("every font-size: Npx value is from the impeccable type scale", () => {
    // Strip the .clarkson rule before checking font sizes.
    // The .clarkson element is a decorative halftone graphic, not text content.
    // Its 5px font-size drives character-cell density, not readability.
    // It is an intentional exemption from the type scale rule.
    const withoutClarkson = template.replace(/\.clarkson\s*\{[^}]*\}/s, "");
    const matches = [...withoutClarkson.matchAll(/font-size:\s*(\d+)px/g)];
    // At least one font-size declaration must be present.
    expect(matches.length).toBeGreaterThan(0);
    for (const match of matches) {
      const size = Number(match[1]);
      expect(VALID_TYPE_SCALE.has(size)).toBe(true);
    }
  });

  test("no font-size value is below 12px", () => {
    // Strip the .clarkson rule before checking minimum sizes.
    // See the note above: .clarkson is a decorative graphic, not text content.
    const withoutClarkson = template.replace(/\.clarkson\s*\{[^}]*\}/s, "");
    const matches = [...withoutClarkson.matchAll(/font-size:\s*(\d+)px/g)];
    for (const match of matches) {
      const size = Number(match[1]);
      expect(size).toBeGreaterThanOrEqual(12);
    }
  });
});
