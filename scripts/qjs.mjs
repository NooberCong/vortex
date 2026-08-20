/**
 * Fetches the QuickJS sidecar — the JavaScript engine the extractor borrows (04 §yt-dlp).
 *
 * YouTube's player challenge is JavaScript, and yt-dlp stopped interpreting it in-process:
 * without a runtime the extraction takes a deprecated path that says out loud that *some
 * formats may be missing*, which reaches a user as a ladder quietly short of its top rungs.
 * yt-dlp only looks for Deno on its own, so a machine with Node and nothing else takes that
 * path silently.
 *
 *   node scripts/qjs.mjs [--target <triple>] [--force]
 *
 * **Why QuickJS and not Deno.** Deno is yt-dlp's default and is a hundred and ten megabytes
 * unpacked. QuickJS is two, does the same job, and is fully supported — `yt-dlp -v` reports
 * it as an available challenge provider. On a per-user installer for a download manager
 * that difference is the whole decision. The engine yt-dlp is *told* about is still the
 * best one present: `JsRuntime::find` prefers a Deno or Node the machine already has.
 *
 * **Why the hashes are written down here.** quickjs-ng publishes no checksum file, so
 * there is nothing to fetch and compare against — and "no published checksum" is not a
 * reason to skip the check on an executable that will run on a user's machine. Instead the
 * version and its hashes are pinned in this file: the bytes were verified once, by hand,
 * and every build since then either matches them or fails. That is a stronger guarantee
 * than a sums file served from the same host as the binary, not a weaker one.
 *
 * Bumping the version means replacing `RELEASE` and every hash under it. `--force` prints
 * what it actually got, which is where the new numbers come from.
 */

import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = "https://github.com/quickjs-ng/quickjs/releases/download";

/**
 * The pinned release.
 *
 * Not "latest", deliberately. A QuickJS older than 2025-04-26 — quickjs-ng before 0.12.0 —
 * is missing optimisations that turn a challenge into *several minutes* of CPU, which as a
 * user-facing symptom is an overlay that never appears rather than an error. Pinning is
 * what keeps that a decision someone made rather than whatever was published this morning.
 */
const RELEASE = "v0.16.1";

/**
 * `rust triple → { asset, sha256 }`.
 *
 * Written out rather than derived: quickjs-ng names its assets by its own platform words,
 * and an unknown target must be an error instead of a guess at a URL.
 */
const ASSETS = {
  "x86_64-pc-windows": {
    asset: "qjs-windows-x86_64.exe",
    sha256: "55a1b69cd4fdb6b0d3f8fdd910d0e89519f5330e408462084140c7b3b964fdae",
  },
  "x86_64-linux": {
    asset: "qjs-linux-x86_64",
    sha256: "aae0d428c88bdd30fb490f54e616ebd4009ec279cc2a16ecebf0c3e17f7e76e7",
  },
  "aarch64-linux": {
    asset: "qjs-linux-aarch64",
    sha256: "c1635453aa60a78ebc7f05b2b559e0e9e9eb7d55b4dfc4a6e71a07d9d10b8a89",
  },
  "x86_64-darwin": {
    asset: "qjs-darwin-x86_64",
    sha256: "5982a1ebb20e1a9bf6162bafd29d445823616084cfeddee8881f8d69d6e0fd74",
  },
  "aarch64-darwin": {
    asset: "qjs-darwin-arm64",
    sha256: "9a24e7435036906c098d539daf47bcc8e7e8ad2f3aa084a0bce9313c6c3527e0",
  },
};

/**
 * Which entry a Rust triple wants.
 *
 * Windows is keyed without its ABI on purpose: `-gnu` and `-msvc` differ in how *Rust*
 * links, and this is a prebuilt C program that neither toolchain compiles. Staging the
 * wrong one for a GNU host is how a sidecar goes missing under a name that is right there.
 */
export function assetFor(target) {
  const arch = target.startsWith("aarch64") || target.startsWith("arm64") ? "aarch64" : "x86_64";
  const platform = target.includes("windows")
    ? "windows"
    : target.includes("darwin") || target.includes("apple")
      ? "darwin"
      : target.includes("linux")
        ? "linux"
        : null;
  if (!platform) throw new Error(`no QuickJS build is published for ${target}`);
  if (platform === "windows" && arch !== "x86_64") {
    // quickjs-ng publishes no Windows arm64 binary. Saying so beats staging an x64 one and
    // letting the user find out.
    throw new Error("quickjs-ng publishes no Windows arm64 build");
  }
  const key = platform === "windows" ? "x86_64-pc-windows" : `${arch}-${platform}`;
  const entry = ASSETS[key];
  if (!entry) throw new Error(`no QuickJS build is published for ${target}`);
  return { ...entry, extension: platform === "windows" ? ".exe" : "" };
}

async function fetchOrDie(url, what) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) throw new Error(`${what}: ${url} answered ${response.status}`);
  return response;
}

export async function fetchQjs({ target, force = false, profile = "release" }) {
  const { asset, sha256: expected, extension } = assetFor(target);
  const out = join(ROOT, "apps", "desktop", "src-tauri", "binaries");
  // `qjs`, not `quickjs`: it is the name the engine ships under, the name yt-dlp documents,
  // and the name `JsRuntime::find` looks for beside the daemon.
  const staged = join(out, `qjs-${target}${extension}`);

  if (!force && existsSync(staged) && sha256(readFileSync(staged)) === expected) {
    console.log(`qjs → ${staged} (already current)`);
    return staged;
  }

  const body = Buffer.from(
    await (await fetchOrDie(`${REPO}/${RELEASE}/${asset}`, "qjs")).arrayBuffer(),
  );
  const actual = sha256(body);
  if (actual !== expected) {
    throw new Error(
      `qjs checksum mismatch for ${asset}: got ${actual}, this script pins ${expected}. ` +
        `If ${RELEASE} was re-cut deliberately, verify the new bytes and update scripts/qjs.mjs.`,
    );
  }

  mkdirSync(out, { recursive: true });
  writeFileSync(staged, body);
  if (extension === "") chmodSync(staged, 0o755);
  console.log(`qjs → ${staged} (${(body.length / 1e6).toFixed(1)} MB, sha256 ${actual.slice(0, 12)}…)`);

  // And beside the daemon a developer actually runs, which is not the bundle.
  for (const dir of [join(ROOT, "target", profile), join(ROOT, "target", target, profile)]) {
    if (!existsSync(dir)) continue;
    const beside = join(dir, `qjs${extension}`);
    copyFileSync(staged, beside);
    if (extension === "") chmodSync(beside, 0o755);
    console.log(`qjs → ${beside}`);
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
  await fetchQjs({
    target: at("--target") ?? hostTriple(),
    force: args.includes("--force"),
    profile: args.includes("--debug") ? "debug" : "release",
  });
}
