import { describe, expect, test } from "bun:test";
import { dedupeFonts, hexToRgb, normalizePadding, solidPaint } from "./helpers";

describe("hexToRgb", () => {
  test("parses a full #RRGGBB string", () => {
    const result = hexToRgb("#ffffff");
    expect(result.r).toBeCloseTo(1, 5);
    expect(result.g).toBeCloseTo(1, 5);
    expect(result.b).toBeCloseTo(1, 5);
  });

  test("parses a RRGGBB string without a hash", () => {
    const result = hexToRgb("000000");
    expect(result.r).toBe(0);
    expect(result.g).toBe(0);
    expect(result.b).toBe(0);
  });

  test("parses a short #RGB string by expanding each digit", () => {
    // #fff -> #ffffff
    const result = hexToRgb("#fff");
    expect(result.r).toBeCloseTo(1, 5);
    expect(result.g).toBeCloseTo(1, 5);
    expect(result.b).toBeCloseTo(1, 5);
  });

  test("parses #3366CC to approximately r=0.2, g=0.4, b=0.8", () => {
    const result = hexToRgb("#3366CC");
    expect(result.r).toBeCloseTo(0.2, 3);
    expect(result.g).toBeCloseTo(0.4, 3);
    expect(result.b).toBeCloseTo(0.8, 3);
  });

  test("returns black for an invalid hex string", () => {
    const result = hexToRgb("not-a-color");
    expect(result).toEqual({ r: 0, g: 0, b: 0 });
  });

  test("returns black for an empty string", () => {
    const result = hexToRgb("");
    expect(result).toEqual({ r: 0, g: 0, b: 0 });
  });

  test("returns black for a partial hex string", () => {
    const result = hexToRgb("#33");
    expect(result).toEqual({ r: 0, g: 0, b: 0 });
  });

  test("all components are in the range 0..1", () => {
    const result = hexToRgb("#7f8081");
    expect(result.r).toBeGreaterThanOrEqual(0);
    expect(result.r).toBeLessThanOrEqual(1);
    expect(result.g).toBeGreaterThanOrEqual(0);
    expect(result.g).toBeLessThanOrEqual(1);
    expect(result.b).toBeGreaterThanOrEqual(0);
    expect(result.b).toBeLessThanOrEqual(1);
  });
});

describe("solidPaint", () => {
  test("returns an object with type SOLID", () => {
    const paint = solidPaint("#000000");
    expect(paint.type).toBe("SOLID");
  });

  test("sets color from the hex string", () => {
    const paint = solidPaint("#ffffff");
    expect(paint.color.r).toBeCloseTo(1, 5);
    expect(paint.color.g).toBeCloseTo(1, 5);
    expect(paint.color.b).toBeCloseTo(1, 5);
  });

  test("defaults opacity to 1", () => {
    const paint = solidPaint("#000000");
    expect(paint.opacity).toBe(1);
  });

  test("applies a custom opacity", () => {
    const paint = solidPaint("#000000", 0.5);
    expect(paint.opacity).toBe(0.5);
  });

  test("returns a plain object with exactly three top-level keys", () => {
    const paint = solidPaint("#aabbcc");
    const keys = Object.keys(paint).sort();
    expect(keys).toEqual(["color", "opacity", "type"]);
  });
});

describe("normalizePadding", () => {
  test("spreads a single number to all four sides", () => {
    const result = normalizePadding(8);
    expect(result).toEqual({ top: 8, right: 8, bottom: 8, left: 8 });
  });

  test("spreads zero to all four sides", () => {
    const result = normalizePadding(0);
    expect(result).toEqual({ top: 0, right: 0, bottom: 0, left: 0 });
  });

  test("fills missing sides with 0 when given a partial object", () => {
    const result = normalizePadding({ top: 10 });
    expect(result).toEqual({ top: 10, right: 0, bottom: 0, left: 0 });
  });

  test("fills all missing sides with 0 when given an empty object", () => {
    const result = normalizePadding({});
    expect(result).toEqual({ top: 0, right: 0, bottom: 0, left: 0 });
  });

  test("preserves all sides when every side is provided", () => {
    const result = normalizePadding({ top: 1, right: 2, bottom: 3, left: 4 });
    expect(result).toEqual({ top: 1, right: 2, bottom: 3, left: 4 });
  });

  test("fills only the missing right and bottom sides", () => {
    const result = normalizePadding({ top: 5, left: 15 });
    expect(result).toEqual({ top: 5, right: 0, bottom: 0, left: 15 });
  });
});

describe("dedupeFonts", () => {
  test("returns the same list when all fonts are unique", () => {
    const fonts = [
      { family: "Inter", style: "Regular" },
      { family: "Inter", style: "Bold" },
      { family: "Roboto", style: "Regular" },
    ];
    const result = dedupeFonts(fonts);
    expect(result).toEqual(fonts);
  });

  test("removes a duplicate font that matches on both family and style", () => {
    const fonts = [
      { family: "Inter", style: "Regular" },
      { family: "Inter", style: "Regular" },
    ];
    const result = dedupeFonts(fonts);
    expect(result).toHaveLength(1);
    expect(result[0]).toEqual({ family: "Inter", style: "Regular" });
  });

  test("keeps the first occurrence when a duplicate appears later", () => {
    const first = { family: "Inter", style: "Regular" };
    const second = { family: "Inter", style: "Bold" };
    const duplicate = { family: "Inter", style: "Regular" };
    const result = dedupeFonts([first, second, duplicate]);
    expect(result).toHaveLength(2);
    expect(result[0]).toBe(first);
    expect(result[1]).toBe(second);
  });

  test("treats same family with different style as distinct", () => {
    const fonts = [
      { family: "Inter", style: "Regular" },
      { family: "Inter", style: "Italic" },
    ];
    const result = dedupeFonts(fonts);
    expect(result).toHaveLength(2);
  });

  test("treats same style with different family as distinct", () => {
    const fonts = [
      { family: "Inter", style: "Regular" },
      { family: "Roboto", style: "Regular" },
    ];
    const result = dedupeFonts(fonts);
    expect(result).toHaveLength(2);
  });

  test("returns an empty list when given an empty list", () => {
    expect(dedupeFonts([])).toEqual([]);
  });

  test("preserves the original order of first occurrences", () => {
    const fonts = [
      { family: "Roboto", style: "Regular" },
      { family: "Inter", style: "Regular" },
      { family: "Roboto", style: "Regular" },
      { family: "Inter", style: "Bold" },
    ];
    const result = dedupeFonts(fonts);
    expect(result).toHaveLength(3);
    expect(result[0].family).toBe("Roboto");
    expect(result[1].family).toBe("Inter");
    expect(result[2]).toEqual({ family: "Inter", style: "Bold" });
  });
});
