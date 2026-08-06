/**
 * Pure halftone conversion functions. No file I/O, no shelling out.
 * All functions are importable and independently testable.
 */

// Read a byte, treating an out-of-range index as 0.
function byteAt(buf: Uint8Array, offset: number): number {
  return buf[offset] ?? 0;
}

// Read a 16-bit unsigned little-endian integer from a byte array.
function readU16LE(buf: Uint8Array, offset: number): number {
  return (byteAt(buf, offset) | (byteAt(buf, offset + 1) << 8)) >>> 0;
}

// Read a 32-bit unsigned little-endian integer from a byte array.
function readU32LE(buf: Uint8Array, offset: number): number {
  return (
    (byteAt(buf, offset) |
      (byteAt(buf, offset + 1) << 8) |
      (byteAt(buf, offset + 2) << 16) |
      (byteAt(buf, offset + 3) << 24)) >>>
    0
  );
}

// Read a 32-bit signed little-endian integer from a byte array.
function readI32LE(buf: Uint8Array, offset: number): number {
  return (
    byteAt(buf, offset) |
    (byteAt(buf, offset + 1) << 8) |
    (byteAt(buf, offset + 2) << 16) |
    (byteAt(buf, offset + 3) << 24)
  );
}

/** Result of parsing a BMP file. Row 0 is always the top of the image. */
export interface BmpData {
  width: number;
  height: number;
  /** lum[row][col]: luminance value 0..255. lum[0] is the top row. */
  lum: number[][];
}

/**
 * Parse a 24-bit or 32-bit uncompressed BMP into per-pixel luminance.
 * Row 0 of the result is the top of the image regardless of storage order.
 * Throws if the file is not a valid uncompressed 24- or 32-bit BMP.
 */
export function parseBmp(buf: Uint8Array): BmpData {
  // Verify the BMP signature.
  if (buf[0] !== 0x42 || buf[1] !== 0x4d) {
    throw new Error("Not a BMP file: missing 'BM' signature.");
  }

  const pixelOffset = readU32LE(buf, 0x0a);
  const width = readI32LE(buf, 0x12);
  const rawHeight = readI32LE(buf, 0x16);
  const bpp = readU16LE(buf, 0x1c);

  // Only 24-bit and 32-bit uncompressed BMPs are supported.
  if (bpp !== 24 && bpp !== 32) {
    throw new Error(`Unsupported bits per pixel: ${bpp}. Expected 24 or 32.`);
  }

  // Negative height means the image is stored top-down.
  const topDown = rawHeight < 0;
  const absHeight = Math.abs(rawHeight);

  // Width must be positive for a valid image.
  if (width <= 0) {
    throw new Error(`Invalid BMP width: ${width}.`);
  }

  // Row size is padded to a 4-byte boundary.
  const rowSize = Math.floor((bpp * width + 31) / 32) * 4;
  const bytesPerPixel = bpp / 8;

  // The pixel data must fit inside the buffer, or the image is truncated.
  if (buf.length < pixelOffset + absHeight * rowSize) {
    throw new Error("Truncated BMP: pixel data runs past the end of the buffer.");
  }

  // Build the luminance grid. Pixels are stored in BGR (or BGRA) order.
  const rows: number[][] = [];
  for (let r = 0; r < absHeight; r++) {
    const rowStart = pixelOffset + r * rowSize;
    const row: number[] = new Array(width) as number[];
    for (let c = 0; c < width; c++) {
      const base = rowStart + c * bytesPerPixel;
      const b = byteAt(buf, base);
      const g = byteAt(buf, base + 1);
      const red = byteAt(buf, base + 2);
      // Rec.601 luminance from linear RGB.
      row[c] = 0.299 * red + 0.587 * g + 0.114 * b;
    }
    rows.push(row);
  }

  // Bottom-up storage: row 0 in the file is the bottom of the image.
  // Reverse so that lum[0] is always the top row.
  if (!topDown) {
    rows.reverse();
  }

  return { width, height: absHeight, lum: rows };
}

/**
 * Map a normalised luminance value (0..1) to a ramp character.
 * norm 0 selects ramp[0] (densest). norm 1 selects the last ramp character.
 * The index is clamped to the ramp bounds.
 */
export function luminanceToChar(norm: number, ramp: string[]): string {
  const clamped = Math.max(0, Math.min(1, norm));
  const raw = Math.round(clamped * (ramp.length - 1));
  const index = Math.max(0, Math.min(raw, ramp.length - 1));
  return ramp[index] ?? " ";
}

/** Options for buildHalftone. */
export interface HalftoneOpts {
  /** Number of character columns in the output. */
  cols: number;
  /** Ramp from densest character (index 0) to lightest (last index). */
  ramp: string[];
  /**
   * Optional gamma applied to the normalised luminance before ramp lookup.
   * Values above 1 darken midtones; values below 1 brighten them.
   * Default is 1 (no correction).
   */
  gamma?: number;
}

/**
 * Downsample a BmpData to a character grid and return the multi-line string.
 * Each output character represents a cell of cellW x cellH source pixels.
 * Aspect correction: cellH = cellW * 2, because characters are ~2x taller than wide.
 * Dark cells produce dense characters; light cells produce spaces.
 */
export function buildHalftone(bmp: BmpData, opts: HalftoneOpts): string {
  const { width, height, lum } = bmp;
  const { cols, ramp, gamma = 1 } = opts;

  // Compute cell dimensions. cellH corrects for character aspect ratio.
  const cellW = width / cols;
  const cellH = cellW * 2;
  const rows = Math.round(height / cellH);

  const lines: string[] = [];
  for (let row = 0; row < rows; row++) {
    let line = "";
    const yStart = Math.round(row * cellH);
    const yEnd = Math.min(Math.round((row + 1) * cellH), height);

    for (let col = 0; col < cols; col++) {
      const xStart = Math.round(col * cellW);
      const xEnd = Math.min(Math.round((col + 1) * cellW), width);

      // Average the luminance of all pixels in the cell.
      let sum = 0;
      let count = 0;
      for (let y = yStart; y < yEnd; y++) {
        const lumRow = lum[y];
        if (!lumRow) continue;
        for (let x = xStart; x < xEnd; x++) {
          const px = lumRow[x];
          if (px !== undefined) {
            sum += px;
            count++;
          }
        }
      }

      // Normalise to 0..1. Dark pixels are near 0.
      let norm = count > 0 ? sum / count / 255 : 0;

      // Apply gamma correction if requested.
      if (gamma !== 1) {
        norm = norm ** gamma;
      }

      line += luminanceToChar(norm, ramp);
    }
    lines.push(line);
  }

  return lines.join("\n");
}
