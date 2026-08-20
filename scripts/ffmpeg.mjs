/**
 * Fetches the `ffmpeg` sidecar — the muxer (04 §mux).
 *
 *   node scripts/ffmpeg.mjs [--target <triple>] [--line n9.0|master] [--force]
 *
 * Vortex asks ffmpeg for exactly one thing: `-c copy` into MP4 or Matroska. It never
 * transcodes. But a separated video and audio track is not a file anybody can play, and
 * every adaptive stream — YouTube especially — arrives separated, so "ffmpeg is missing"
 * and "streaming does not work" are the same sentence to a user. It ships in the box.
 *
 * **It is the largest thing in the bundle by a wide margin** — a static ffmpeg is ~115 MB
 * against the daemon's 21 — and that is worth being deliberate about:
 *
 * - **LGPL, not GPL.** The GPL builds add encoders (x264, x265) that a program doing
 *   `-c copy` will never call, and take the whole bundle's licence with them.
 * - **Static, not `-shared`.** A shared build is a small `ffmpeg.exe` beside forty DLLs.
 *   Tauri's `externalBin` stages one file per entry, and `Ffmpeg::find` looks for one file
 *   beside the daemon; a loose DLL set would be a new kind of thing for both to understand.
 * - **A release line, not master.** `n9.0` is a maintenance branch. `--line master` exists
 *   for when a fix has landed and not yet shipped.
 *
 * **macOS is not covered and the failure is loud.** BtbN publishes Windows and Linux only,
 * and the macOS builds that exist elsewhere are either unsigned with no published hash or
 * Intel-only. Rather than ship an executable this script cannot verify, a Darwin target is
 * refused here and `binaries/ffmpeg` is absent from `tauri.macos.conf.json`'s `externalBin`
 * — so a macOS bundle builds exactly as it did before, and `Ffmpeg::find` falls through to
 * `VORTEX_FFMPEG` and `PATH`.
 */

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { inflateRawSync } from "node:zlib";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";
const SUMS = "checksums.sha256";

/** BtbN's word for a platform, by Rust triple. `null` where it publishes nothing. */
function platformFor(target) {
  const arm = target.startsWith("aarch64") || target.startsWith("arm64");
  if (target.includes("windows")) return arm ? "winarm64" : "win64";
  if (target.includes("linux")) return arm ? "linuxarm64" : "linux64";
  return null;
}

async function fetchOrDie(url, what) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) throw new Error(`${what}: ${url} answered ${response.status}`);
  return response;
}

/**
 * Picks the asset out of the release's own checksum list.
 *
 * The list is the authority on both halves of the question — which file exists and what it
 * hashes to — so there is no second source to disagree with it, and no asset name assembled
 * here out of guesses about how the version is spelled in the filename this month.
 */
export async function resolveAsset(target, line) {
  const platform = platformFor(target);
  if (!platform) {
    throw new Error(
      `no ffmpeg build is published for ${target}. ` +
        `Windows and Linux are covered; a macOS bundle leaves ffmpeg to PATH by design.`,
    );
  }
  const text = await (await fetchOrDie(`${REPO}/${SUMS}`, "checksum list")).text();
  const wanted = new RegExp(`^ffmpeg-${line}-latest-${platform}-lgpl(-[\\d.]+)?\\.(zip|tar\\.xz)$`);

  for (const raw of text.split("\n")) {
    // `<sha256>  <filename>`, two spaces, as `sha256sum` writes it.
    const [hash, name] = raw.trim().split(/\s+/);
    if (hash && name && wanted.test(name)) return { asset: name, sha256: hash.toLowerCase() };
  }
  throw new Error(`no ${line} LGPL build for ${platform} is listed in ${SUMS}`);
}

/**
 * Reads one named member out of a zip, without unpacking the other 150 MB.
 *
 * Written out rather than shelled out to because the two platforms that need it disagree
 * about what is installed: `unzip` is not on a stock Windows, and PowerShell's
 * `Expand-Archive` has no way to ask for a single entry. Deflate and stored are the only
 * two methods any of these archives use.
 */
function readFromZip(zip, matches) {
  const eocd = zip.lastIndexOf(Buffer.from("PK\x05\x06"));
  if (eocd < 0) throw new Error("not a zip: no end-of-central-directory record");
  const count = zip.readUInt16LE(eocd + 10);
  let at = zip.readUInt32LE(eocd + 16);

  for (let i = 0; i < count; i++) {
    if (zip.readUInt32LE(at) !== 0x02014b50) throw new Error("corrupt central directory");
    const method = zip.readUInt16LE(at + 10);
    const compressed = zip.readUInt32LE(at + 20);
    const nameLength = zip.readUInt16LE(at + 28);
    const extraLength = zip.readUInt16LE(at + 30);
    const commentLength = zip.readUInt16LE(at + 32);
    const localAt = zip.readUInt32LE(at + 42);
    const name = zip.toString("utf8", at + 46, at + 46 + nameLength);

    if (matches(name)) {
      // The local header repeats the name and extra fields, at its own lengths.
      const localNameLength = zip.readUInt16LE(localAt + 26);
      const localExtraLength = zip.readUInt16LE(localAt + 28);
      const from = localAt + 30 + localNameLength + localExtraLength;
      const body = zip.subarray(from, from + compressed);
      if (method === 0) return { name, body };
      if (method === 8) return { name, body: inflateRawSync(body) };
      throw new Error(`${name} uses compression method ${method}`);
    }
    at += 46 + nameLength + extraLength + commentLength;
  }
  throw new Error("no matching entry in the archive");
}

/**
 * The same, for a `.tar.xz`.
 *
 * Shelled out to `tar` here and not there: every Linux has one, it reads xz, and Node does
 * not — `zlib` has no LZMA. The member is listed first and then extracted by exact name, so
 * nothing depends on `--wildcards`, which is GNU-only.
 */
function readFromTarXz(archivePath, matches) {
  const listing = execFileSync("tar", ["-tJf", archivePath], { encoding: "utf8", maxBuffer: 1 << 26 });
  const member = listing.split("\n").map((l) => l.trim()).find(matches);
  if (!member) throw new Error("no matching entry in the archive");

  const scratch = mkdtempSync(join(tmpdir(), "vortex-ffmpeg-"));
  try {
    execFileSync("tar", ["-xJf", archivePath, "-C", scratch, member]);
    return { name: member, body: readFileSync(join(scratch, member)) };
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/** What the last successful run produced, so a 150 MB download is not repeated. */
const receiptFor = (out, target) => join(out, `.ffmpeg-${target}.json`);

export async function fetchFfmpeg({ target, line = "n9.0", force = false, profile = "release" }) {
  const windows = target.includes("windows");
  const extension = windows ? ".exe" : "";
  const out = join(ROOT, "apps", "desktop", "src-tauri", "binaries");
  const staged = join(out, `ffmpeg-${target}${extension}`);

  const { asset, sha256: expected } = await resolveAsset(target, line);

  // The published hash covers the *archive*; what is staged is one file out of it. So the
  // receipt records both, and the staged binary is re-verified against its own hash — a
  // truncated or edited copy is replaced rather than trusted for being the right size.
  const receipt = receiptFor(out, target);
  if (!force && existsSync(staged) && existsSync(receipt)) {
    try {
      const previous = JSON.parse(readFileSync(receipt, "utf8"));
      if (previous.archiveSha256 === expected && sha256(readFileSync(staged)) === previous.binarySha256) {
        console.log(`ffmpeg → ${staged} (already current)`);
        return staged;
      }
    } catch {
      // An unreadable receipt is a reason to fetch again, not to fail.
    }
  }

  console.log(`ffmpeg ← ${asset} (this one is large; it is cached afterwards)`);
  const archive = Buffer.from(await (await fetchOrDie(`${REPO}/${asset}`, "ffmpeg")).arrayBuffer());
  const actual = sha256(archive);
  if (actual !== expected) {
    throw new Error(`ffmpeg checksum mismatch: got ${actual}, ${SUMS} says ${expected}`);
  }

  // Matched by shape rather than by the full path, because the directory inside the archive
  // carries the version and the version moves.
  const wanted = (name) => name.endsWith(`/bin/ffmpeg${extension}`);
  let extracted;
  if (asset.endsWith(".zip")) {
    extracted = readFromZip(archive, wanted);
  } else {
    const scratch = mkdtempSync(join(tmpdir(), "vortex-ffmpeg-dl-"));
    const archivePath = join(scratch, asset);
    try {
      writeFileSync(archivePath, archive);
      extracted = readFromTarXz(archivePath, wanted);
    } finally {
      rmSync(scratch, { recursive: true, force: true });
    }
  }

  mkdirSync(out, { recursive: true });
  writeFileSync(staged, extracted.body);
  if (!windows) chmodSync(staged, 0o755);
  const binarySha256 = sha256(extracted.body);
  writeFileSync(
    receipt,
    `${JSON.stringify({ asset, archiveSha256: expected, member: extracted.name, binarySha256 }, null, 2)}\n`,
  );
  console.log(
    `ffmpeg → ${staged} (${(extracted.body.length / 1e6).toFixed(1)} MB, sha256 ${binarySha256.slice(0, 12)}…)`,
  );

  // And beside the daemon a developer actually runs, which is not the bundle.
  for (const dir of [join(ROOT, "target", profile), join(ROOT, "target", target, profile)]) {
    if (!existsSync(dir)) continue;
    const beside = join(dir, `ffmpeg${extension}`);
    copyFileSync(staged, beside);
    if (!windows) chmodSync(beside, 0o755);
    console.log(`ffmpeg → ${beside}`);
  }
  return staged;
}

function sha256(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const at = (flag) => (args.includes(flag) ? args[args.indexOf(flag) + 1] : undefined);
  const { hostTriple } = await import("./sidecars.mjs");
  await fetchFfmpeg({
    target: at("--target") ?? hostTriple(),
    line: at("--line") ?? "n9.0",
    force: args.includes("--force"),
    profile: args.includes("--debug") ? "debug" : "release",
  });
}
