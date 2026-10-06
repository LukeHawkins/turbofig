/**
 * Scales the alpha channel of an 8-bit RGBA PNG and writes a new PNG.
 *
 * Usage: bun scripts/dim-png.ts <input.png> <output.png> <alpha factor 0..1>
 *
 * scripts/make-tray-icon.sh uses this to make the dimmed menu-bar icon. It has
 * no dependencies: it decodes the PNG scanlines itself, so it works with only
 * Bun installed (Swift and Python image libraries are not reliable on every
 * Mac). Supports non-interlaced 8-bit RGBA PNGs, which is what `sips -s format
 * png` writes for a black-on-transparent glyph.
 */
import { deflateSync, inflateSync } from "node:zlib";

const [inputPath, outputPath, factorArg] = process.argv.slice(2);
if (!inputPath || !outputPath || !factorArg) {
  console.error("usage: bun scripts/dim-png.ts <input.png> <output.png> <factor>");
  process.exit(1);
}
const factor = Number(factorArg);
if (!(factor >= 0 && factor <= 1)) {
  console.error(`dim-png: factor must be between 0 and 1, got ${factorArg}`);
  process.exit(1);
}

const png = Buffer.from(await Bun.file(inputPath).arrayBuffer());
const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
if (!png.subarray(0, 8).equals(SIGNATURE)) {
  console.error(`dim-png: ${inputPath} is not a PNG`);
  process.exit(1);
}

let width = 0;
let height = 0;
const idat: Buffer[] = [];
for (let offset = 8; offset < png.length; ) {
  const length = png.readUInt32BE(offset);
  const type = png.toString("latin1", offset + 4, offset + 8);
  const data = png.subarray(offset + 8, offset + 8 + length);
  if (type === "IHDR") {
    width = data.readUInt32BE(0);
    height = data.readUInt32BE(4);
    const bitDepth = data[8];
    const colorType = data[9];
    const interlace = data[12];
    if (bitDepth !== 8 || colorType !== 6 || interlace !== 0) {
      console.error(
        `dim-png: need a non-interlaced 8-bit RGBA PNG (got depth ${bitDepth}, colour type ${colorType}, interlace ${interlace})`,
      );
      process.exit(1);
    }
  } else if (type === "IDAT") {
    idat.push(data);
  }
  offset += 12 + length;
}

const bpp = 4;
const stride = width * bpp;
const raw = inflateSync(Buffer.concat(idat));
const pixels = Buffer.alloc(stride * height);

// Undo the per-scanline PNG filters (None, Sub, Up, Average, Paeth).
for (let y = 0; y < height; y++) {
  const filter = raw[y * (stride + 1)];
  const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
  const out = pixels.subarray(y * stride, (y + 1) * stride);
  const prev = y > 0 ? pixels.subarray((y - 1) * stride, y * stride) : Buffer.alloc(stride);
  for (let x = 0; x < stride; x++) {
    const a = x >= bpp ? out[x - bpp] : 0;
    const b = prev[x];
    const c = x >= bpp ? prev[x - bpp] : 0;
    let predictor = 0;
    if (filter === 1) predictor = a;
    else if (filter === 2) predictor = b;
    else if (filter === 3) predictor = (a + b) >> 1;
    else if (filter === 4) {
      const p = a + b - c;
      const pa = Math.abs(p - a);
      const pb = Math.abs(p - b);
      const pc = Math.abs(p - c);
      predictor = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
    } else if (filter !== 0) {
      console.error(`dim-png: unknown PNG filter type ${filter}`);
      process.exit(1);
    }
    out[x] = (line[x] + predictor) & 0xff;
  }
}

for (let i = 3; i < pixels.length; i += bpp) {
  pixels[i] = Math.round(pixels[i] * factor);
}

// Re-encode with filter type None on every scanline.
const filtered = Buffer.alloc((stride + 1) * height);
for (let y = 0; y < height; y++) {
  pixels.copy(filtered, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
}

const CRC_TABLE = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(bytes: Buffer): number {
  let c = 0xffffffff;
  for (const byte of bytes) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type: string, data: Buffer): Buffer {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(data.length, 0);
  head.write(type, 4, "latin1");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])), 0);
  return Buffer.concat([head, data, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(width, 0);
ihdr.writeUInt32BE(height, 4);
ihdr[8] = 8;
ihdr[9] = 6;
await Bun.write(
  outputPath,
  Buffer.concat([
    SIGNATURE,
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(filtered)),
    chunk("IEND", Buffer.alloc(0)),
  ]),
);
