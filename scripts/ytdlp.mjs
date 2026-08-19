/**
 * Fetches the `yt-dlp` sidecar (04 §yt-dlp as the extractor fallback).
 *
 * The extractor is the one part of Vortex that cannot ship on Vortex's release cadence.
 * Sites change their obfuscation weekly and yt-dlp answers within days, so it is bundled
 * rather than vendored: `apps/desktop/src-tauri/binaries/` is gitignored, and this script
 * puts a verified copy there at build time exactly the way `sidecars.mjs` puts the two
 * binaries cargo builds. Nothing third-party enters the repository.
 *
 *   node scripts/ytdlp.mjs [--target <triple>] [--tag <release>] [--force]
 *
 * **The checksum is not optional.** This downloads an executable that will be run on a
 * user's machine with their network identity, so the release's own `SHA2-256SUMS` is
 * fetched alongside it and a mismatch is a hard failure, not a warning. GitHub is asked
 * for the two files over TLS and nothing else is trusted.
 *
 * A copy also lands beside the freshly built `vortexd` in `target/`, because
 * `YtDlp::find()` looks next to the calling executable first and a developer running the
 * daemon out of `target/release` is not running it out of a Tauri bundle.
 */

import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = "https://github.com/yt-dlp/yt-dlp/releases";
const SUMS = "SHA2-256SUMS";

/**
 * Which asset a target triple wants.
 *
 * yt-dlp publishes one self-contained build per platform under a name of its own choosing,
 * and the names have nothing to do with Rust's triples — so the mapping is written out
 * rather than derived, and an unknown platform is an error instead of a guess.
 */
function assetFor(target) {
  if (target.includes("windows")) return { asset: "yt-dlp.exe", extension: ".exe" };
  if (target.includes("darwin") || target.includes("apple")) {
    return { asset: "yt-dlp_macos", extension: "" };
  }
  if (target.includes("linux")) {
    return { asset: target.startsWith("aarch64") ? "yt-dlp_linux_aarch64" : "yt-dlp_linux", extension: "" };
  }
  throw new Error(`no yt-dlp build is published for ${target}`);
}

async function fetchOrDie(url, what) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) throw new Error(`${what}: ${url} answered ${response.status}`);
  return response;
}

/** The hash the release itself claims for this asset. */
async function publishedHash(base, asset) {
  const text = await (await fetchOrDie(`${base}/${SUMS}`, "checksum list")).text();
  for (const line of text.split("\n")) {
    // `<sha256>  <filename>`, two spaces, as `sha256sum` writes it.
    const [hash, name] = line.trim().split(/\s+/);
    if (name === asset && hash) return hash.toLowerCase();
  }
  throw new Error(`${asset} is not listed in ${SUMS}`);
}

export async function fetchYtDlp({ target, tag = "latest", force = false, profile = "release" }) {
  const { asset, extension } = assetFor(target);
  const out = join(ROOT, "apps", "desktop", "src-tauri", "binaries");
  const staged = join(out, `yt-dlp-${target}${extension}`);

  const base = tag === "latest" ? `${REPO}/latest/download` : `${REPO}/download/${tag}`;
  const expected = await publishedHash(base, asset);

  // Already the right bytes. The check is the hash rather than the mtime, so a partial or
  // tampered-with file from an interrupted run is replaced rather than trusted.
  if (!force && existsSync(staged) && sha256(readFileSync(staged)) === expected) {
    console.log(`yt-dlp → ${staged} (already current)`);
    return staged;
  }

  const body = Buffer.from(await (await fetchOrDie(`${base}/${asset}`, "yt-dlp")).arrayBuffer());
  const actual = sha256(body);
  if (actual !== expected) {
    throw new Error(`yt-dlp checksum mismatch: got ${actual}, ${SUMS} says ${expected}`);
  }

  mkdirSync(out, { recursive: true });
  writeFileSync(staged, body);
  if (extension === "") chmodSync(staged, 0o755);
  console.log(`yt-dlp → ${staged} (${(body.length / 1e6).toFixed(1)} MB, sha256 ${actual.slice(0, 12)}…)`);

  // And beside the daemon a developer actually runs, which is not the bundle.
  for (const dir of [join(ROOT, "target", profile), join(ROOT, "target", target, profile)]) {
    if (!existsSync(dir)) continue;
    const beside = join(dir, `yt-dlp${extension}`);
    copyFileSync(staged, beside);
    if (extension === "") chmodSync(beside, 0o755);
    console.log(`yt-dlp → ${beside}`);
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
  await fetchYtDlp({
    target: at("--target") ?? hostTriple(),
    tag: at("--tag") ?? "latest",
    force: args.includes("--force"),
    profile: args.includes("--debug") ? "debug" : "release",
  });
}
