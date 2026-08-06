/**
 * Tests for the pure halftone conversion functions.
 * All tests operate on synthetic in-memory data. No file I/O.
 */

import { describe, expect, test } from "bun:test";
import { buildHalftone, luminanceToChar, parseBmp } from "./halftone.ts";

// ---------------------------------------------------------------------------
// BMP construction helpers
// ---------------------------------------------------------------------------

/**
 * Build a minimal 24-bit uncompressed BMP byte array in memory.
 * pixels: an array of rows (top to bottom in logical image order).
 *   Each row is an array of { r, g, b } values.
 * topDown: if true, the BMP height field is negative (top-down storage).
 *   The pixel rows are written in the same order as the input array.
 *   If false (default), rows are written bottom-up (reversed in the file).
 */
function makeBmp24(
  pixels: Array<Array<{ r: number; g: number; b: number }>>,
  topDown = false,
): Uint8Array {
  const height = pixels.length;
  const width = pixels[0]?.length ?? 0;
  // Row size is padded to a 4-byte boundary.
  const rowSize = Math.floor((24 * width + 31) / 32) * 4;
  const pixelDataSize = rowSize * height;
  const fileSize = 54 + pixelDataSize;

  const buf = new Uint8Array(fileSize);
  const view = new DataView(buf.buffer);

  // File header (14 bytes).
  buf[0] = 0x42; // 'B'
  buf[1] = 0x4d; // 'M'
  view.setUint32(2, fileSize, true); // file size
  view.setUint32(6, 0, true); // reserved
  view.setUint32(10, 54, true); // pixel data offset

  // DIB header — BITMAPINFOHEADER (40 bytes).
  view.setUint32(14, 40, true); // header size
  view.setInt32(18, width, true); // width
  // Negative height signals top-down storage.
  view.setInt32(22, topDown ? -height : height, true);
  view.setUint16(26, 1, true); // color planes
  view.setUint16(28, 24, true); // bits per pixel
  view.setUint32(30, 0, true); // compression (0 = none)
  view.setUint32(34, pixelDataSize, true); // image size

  // Pixel data: BMP stores rows bottom-up by default.
  // Reverse the rows for bottom-up storage; keep order for top-down.
  const fileRows = topDown ? [...pixels] : [...pixels].reverse();

  for (let r = 0; r < height; r++) {
    const row = fileRows[r]!;
    const rowStart = 54 + r * rowSize;
    for (let c = 0; c < width; c++) {
      const px = row[c]!;
      const base = rowStart + c * 3;
      buf[base] = px.b; // BMP uses BGR order
      buf[base + 1] = px.g;
      buf[base + 2] = px.r;
    }
    // Remaining bytes in the row are already zero-padded by Uint8Array.
  }

  return buf;
}

// ---------------------------------------------------------------------------
// parseBmp tests
// ---------------------------------------------------------------------------

describe("parseBmp", () => {
  test("parses a 2x2 bottom-up 24-bit BMP and places top row at lum[0]", () => {
    // Define pixels top-to-bottom in logical image order.
    // Top row:    blue (B=255,G=0,R=0) | white (B=255,G=255,R=255)
    // Bottom row: red  (B=0,G=0,R=255) | green (B=0,G=255,R=0)
    const pixels = [
      [
        { r: 0, g: 0, b: 255 }, // blue
        { r: 255, g: 255, b: 255 }, // white
      ],
      [
        { r: 255, g: 0, b: 0 }, // red
        { r: 0, g: 255, b: 0 }, // green
      ],
    ];

    // makeBmp24 writes bottom-up by default (topDown = false).
    const buf = makeBmp24(pixels);
    const result = parseBmp(buf);

    expect(result.width).toBe(2);
    expect(result.height).toBe(2);

    // lum[0] must be the top row (blue | white).
    // blue: lum = 0.114 * 255 ≈ 29.07
    expect(result.lum[0]![0]).toBeCloseTo(0.114 * 255, 1);
    // white: lum = 255
    expect(result.lum[0]![1]).toBeCloseTo(255, 1);

    // lum[1] must be the bottom row (red | green).
    // red: lum = 0.299 * 255 ≈ 76.245
    expect(result.lum[1]![0]).toBeCloseTo(0.299 * 255, 1);
    // green: lum = 0.587 * 255 ≈ 149.685
    expect(result.lum[1]![1]).toBeCloseTo(0.587 * 255, 1);
  });

  test("parses a 2x2 top-down 24-bit BMP (negative height) correctly", () => {
    const pixels = [
      [
        { r: 0, g: 0, b: 255 }, // blue — top-left
        { r: 255, g: 255, b: 255 }, // white — top-right
      ],
      [
        { r: 255, g: 0, b: 0 }, // red — bottom-left
        { r: 0, g: 255, b: 0 }, // green — bottom-right
      ],
    ];

    // topDown = true: height field is negative in the file.
    const buf = makeBmp24(pixels, true);
    const result = parseBmp(buf);

    expect(result.width).toBe(2);
    expect(result.height).toBe(2);

    // Row order must match the input (top-down stored, so no reversal).
    expect(result.lum[0]![0]).toBeCloseTo(0.114 * 255, 1); // blue
    expect(result.lum[0]![1]).toBeCloseTo(255, 1); // white
    expect(result.lum[1]![0]).toBeCloseTo(0.299 * 255, 1); // red
    expect(result.lum[1]![1]).toBeCloseTo(0.587 * 255, 1); // green
  });

  test("throws a clear error for an unsupported bits-per-pixel value", () => {
    // Construct a header with bpp = 8 (paletted; not supported).
    const buf = makeBmp24([[{ r: 128, g: 128, b: 128 }]]);
    // Overwrite the bpp field (offset 0x1c = 28) with 8.
    const view = new DataView(buf.buffer);
    view.setUint16(28, 8, true);
    expect(() => parseBmp(buf)).toThrow("Unsupported bits per pixel: 8");
  });
});

// ---------------------------------------------------------------------------
// luminanceToChar tests
// ---------------------------------------------------------------------------

describe("luminanceToChar", () => {
  const ramp = ["o", "-", ".", " "];

  test("norm 0 returns the densest ramp character", () => {
    expect(luminanceToChar(0, ramp)).toBe("o");
  });

  test("norm 1 returns the lightest ramp character (space)", () => {
    expect(luminanceToChar(1, ramp)).toBe(" ");
  });

  test("a mid-range norm maps to a middle character", () => {
    // norm ≈ 0.33 -> index = round(0.33 * 3) = round(0.99) = 1 -> "-"
    expect(luminanceToChar(0.33, ramp)).toBe("-");
    // norm = 0.5 -> index = round(0.5 * 3) = round(1.5) = 2 -> "."
    expect(luminanceToChar(0.5, ramp)).toBe(".");
  });

  test("values below 0 clamp to the densest character", () => {
    expect(luminanceToChar(-1, ramp)).toBe("o");
  });

  test("values above 1 clamp to the lightest character", () => {
    expect(luminanceToChar(2, ramp)).toBe(" ");
  });
});

// ---------------------------------------------------------------------------
// buildHalftone tests
// ---------------------------------------------------------------------------

describe("buildHalftone", () => {
  // Synthetic 4x8 BMP: left half is pure black (lum=0), right half pure white (lum=255).
  // cols=2: cellW=2, cellH=4, rows=2.
  const syntheticBmp = {
    width: 4,
    height: 8,
    lum: Array.from({ length: 8 }, () => [0, 0, 255, 255]) as number[][],
  };

  const ramp = ["o", "-", ".", " "];

  test("output has the correct number of rows", () => {
    const result = buildHalftone(syntheticBmp, { cols: 2, ramp });
    const lines = result.split("\n");
    // cellW=2, cellH=4, rows=round(8/4)=2
    expect(lines.length).toBe(2);
  });

  test("each output row has the correct number of columns", () => {
    const result = buildHalftone(syntheticBmp, { cols: 2, ramp });
    for (const line of result.split("\n")) {
      expect(line.length).toBe(2);
    }
  });

  test("dark cells produce the densest character", () => {
    const result = buildHalftone(syntheticBmp, { cols: 2, ramp });
    const lines = result.split("\n");
    // Column 0 covers x=[0,1] which are all-zero (pure black).
    for (const line of lines) {
      expect(line[0]).toBe("o");
    }
  });

  test("light cells produce a space", () => {
    const result = buildHalftone(syntheticBmp, { cols: 2, ramp });
    const lines = result.split("\n");
    // Column 1 covers x=[2,3] which are all-255 (pure white).
    for (const line of lines) {
      expect(line[1]).toBe(" ");
    }
  });

  test("gamma correction shifts midtone cells toward denser characters", () => {
    // A 50% grey BMP: all cells are lum=128 (norm≈0.502).
    const greyBmp = {
      width: 4,
      height: 4,
      lum: Array.from({ length: 4 }, () => [128, 128, 128, 128]) as number[][],
    };
    // Without gamma: norm ≈ 0.502 -> index = round(0.502*3) = round(1.506) = 2 -> "."
    const noGamma = buildHalftone(greyBmp, { cols: 2, ramp, gamma: 1 });
    // With high gamma (4): norm^4 ≈ 0.064 -> index = round(0.064*3) ≈ 0 -> "o"
    const highGamma = buildHalftone(greyBmp, { cols: 2, ramp, gamma: 4 });
    // High gamma must produce a denser character than no gamma.
    const noGammaChar = noGamma.split("\n")[0]![0]!;
    const highGammaChar = highGamma.split("\n")[0]![0]!;
    const densityOf = (c: string) => ramp.indexOf(c);
    expect(densityOf(highGammaChar)).toBeLessThan(densityOf(noGammaChar));
  });
});
