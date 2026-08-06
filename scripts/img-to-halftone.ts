/**
 * macOS dev-only regeneration script.
 * Converts clarkson-source.jpg to a halftone text file using macOS sips.
 * Run with: bun scripts/img-to-halftone.ts
 *
 * Requires: sips (available on macOS by default).
 * The build does NOT run this script. It reads the committed clarkson.txt.
 */

import { execFileSync } from "node:child_process";
import { readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildHalftone, parseBmp } from "./halftone.ts";

const SRC = join(import.meta.dir, "../plugin/assets/clarkson-source.jpg");
const OUT = join(import.meta.dir, "../plugin/assets/clarkson.txt");
const TMP = join(tmpdir(), "clarkson-halftone-tmp.bmp");

// Convert the source JPEG to a 24-bit BMP with sips (macOS built-in tool).
console.log("Converting source image to BMP with sips…");
execFileSync("sips", ["-s", "format", "bmp", SRC, "--out", TMP]);

// Read and parse the BMP.
const bmpBytes = new Uint8Array(readFileSync(TMP));
const bmp = parseBmp(bmpBytes);
console.log(`Parsed BMP: ${bmp.width}x${bmp.height}`);

// Remove the temporary BMP file.
try {
  unlinkSync(TMP);
} catch {
  // Non-fatal: temp file cleanup failure does not affect the output.
}

// Build the halftone text.
// cols=84: cellW ≈ 22.9px, cellH ≈ 45.7px -> ~24 rows.
// ramp: densest ("o") to lightest (" ") for ink-on-paper on a white panel.
// gamma=1.1: slightly darkens midtones so Clarkson's face reads clearly.
const halftone = buildHalftone(bmp, {
  cols: 84,
  ramp: ["o", "-", ".", " "],
  gamma: 1.1,
});

const lineCount = halftone.split("\n").length;
const colCount = halftone.split("\n")[0]?.length ?? 0;
console.log(`Halftone grid: ${colCount} cols x ${lineCount} rows`);

// Write the result to the plugin assets directory.
writeFileSync(OUT, halftone, "utf-8");
console.log(`Written: ${OUT}`);
