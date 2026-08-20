/**
 * Renders the listing screenshots.
 *
 *   node store-assets/render.mjs
 *
 * Headless Chrome at exactly 1280x800, then a re-encode to 24-bit RGB, because the store
 * accepts "JPEG or 24-bit PNG (no alpha)" and Chrome writes RGBA. Rejecting the upload
 * over a channel nobody can see is the kind of thing that costs an afternoon.
 *
 * The browser is given a throwaway profile directory. It must never be pointed at the
 * user's own: a screenshot tool that opens someone's real profile can put their history,
 * their session and their name into a picture that goes on a public listing.
 */

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { deflateSync, inflateSync } from "node:zlib";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "out");

const PAGES = ["1-handover", "2-parallel", "3-per-site", "4-private"];
const WIDTH = 1280;
const HEIGHT = 800;

const CHROME = [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
  "/usr/bin/google-chrome",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
].find((path) => existsSync(path));

if (!CHROME) throw new Error("Chrome not found; add its path to CHROME in render.mjs");

mkdirSync(out, { recursive: true });
const profile = mkdtempSync(join(tmpdir(), "vortex-shot-"));

try {
  for (const page of PAGES) {
    const file = join(out, `${page}.png`);
    execFileSync(
      CHROME,
      [
        "--headless=new",
        "--disable-gpu",
        "--hide-scrollbars",
        "--force-device-scale-factor=1",
        `--window-size=${WIDTH},${HEIGHT}`,
        // The pages load the extension's own stylesheet, the tokens and the font files
        // from elsewhere in the workspace, and every file:// document is its own origin.
        "--allow-file-access-from-files",
        // Long enough for the web fonts to arrive; `font-display: block` holds the text
        // back until they do, so a short budget would screenshot blank lines.
        "--virtual-time-budget=5000",
        `--user-data-dir=${profile}`,
        "--no-first-run",
        "--no-default-browser-check",
        `--screenshot=${file}`,
        pathToFileURL(join(here, `${page}.html`)).href,
      ],
      { stdio: "pipe" },
    );

    const flat = flatten(readFileSync(file));
    writeFileSync(file, flat.png);
    console.log(`${page}.png  ${flat.width}x${flat.height}  24-bit RGB, no alpha`);
  }
} finally {
  rmSync(profile, { recursive: true, force: true });
}

/** RGBA PNG in, 24-bit RGB PNG out, composited over white. */
function flatten(buffer) {
  const { width, height, colorType, bitDepth, interlace, data } = decode(buffer);
  if (bitDepth !== 8 || interlace !== 0 || (colorType !== 6 && colorType !== 2)) {
    throw new Error(`unexpected PNG: depth ${bitDepth}, colour type ${colorType}`);
  }
  if (colorType === 2) return { width, height, png: buffer };

  const rgb = Buffer.alloc(width * height * 3);
  for (let i = 0, o = 0; i < data.length; i += 4, o += 3) {
    const a = data[i + 3] / 255;
    // Straight alpha over white. Every page here is opaque, so this is belt and braces —
    // but a half-transparent shadow silently darkening on upload would be worse.
    rgb[o] = Math.round(data[i] * a + 255 * (1 - a));
    rgb[o + 1] = Math.round(data[i + 1] * a + 255 * (1 - a));
    rgb[o + 2] = Math.round(data[i + 2] * a + 255 * (1 - a));
  }
  return { width, height, png: encode(rgb, width, height) };
}

function decode(buffer) {
  let at = 8; // past the signature
  const idat = [];
  let header;
  while (at < buffer.length) {
    const length = buffer.readUInt32BE(at);
    const type = buffer.toString("ascii", at + 4, at + 8);
    const body = buffer.subarray(at + 8, at + 8 + length);
    if (type === "IHDR") {
      header = {
        width: body.readUInt32BE(0),
        height: body.readUInt32BE(4),
        bitDepth: body[8],
        colorType: body[9],
        interlace: body[12],
      };
    } else if (type === "IDAT") idat.push(body);
    else if (type === "IEND") break;
    at += 12 + length;
  }
  const channels = header.colorType === 6 ? 4 : 3;
  return { ...header, data: unfilter(inflateSync(Buffer.concat(idat)), header, channels) };
}

/** Undoes the per-row filters. The five of them are the whole of PNG's compression trick. */
function unfilter(raw, { width, height }, channels) {
  const stride = width * channels;
  const out = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? out[y * stride + x - channels] : 0;
      const b = y > 0 ? out[(y - 1) * stride + x] : 0;
      const c = x >= channels && y > 0 ? out[(y - 1) * stride + x - channels] : 0;
      let value = line[x];
      if (filter === 1) value += a;
      else if (filter === 2) value += b;
      else if (filter === 3) value += (a + b) >> 1;
      else if (filter === 4) value += paeth(a, b, c);
      out[y * stride + x] = value & 0xff;
    }
  }
  return out;
}

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

function encode(rgb, width, height) {
  const stride = width * 3;
  const raw = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0; // filter: none. These images are flat; it costs little.
    rgb.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // colour type: truecolour, no alpha
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function chunk(type, body) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(body.length, 0);
  const typed = Buffer.concat([Buffer.from(type, "ascii"), body]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(typed), 0);
  return Buffer.concat([length, typed, crc]);
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
