/**
 * Builds the installer.
 *
 *   node scripts/bundle.mjs [-- <extra tauri build args>]
 *
 * Two steps that have to agree on one thing. The sidecars are staged under a target
 * triple, and Tauri looks for them under a target triple, and if those differ the bundle
 * fails with a file-not-found for a file that exists — so the triple is worked out once,
 * from `rustc`, and handed to both.
 *
 * The result is a **per-user** installer: no admin, no `Program Files`, no service. That
 * is a constraint from 01 §Process model rather than a packaging preference, and it is why
 * the target is NSIS and not WiX. Tauri's MSI is per-machine only, which would take
 * elevation at install time and buy nothing — `vortexd` runs in the user's session on
 * purpose, so that it has the user's proxy settings and certificate store.
 */

import { createRequire } from "node:module";
import { join } from "node:path";

import { ROOT, hostTriple, run, stage } from "./sidecars.mjs";
import { fetchFfmpeg } from "./ffmpeg.mjs";
import { fetchQjs } from "./qjs.mjs";
import { fetchYtDlp } from "./ytdlp.mjs";

const passthrough = process.argv.slice(2);
const target = hostTriple();

stage(target);

// The three sidecars cargo does not build. None is vendored: each is fetched at build time
// and checked against a hash before it is staged, because all three end up running on a
// user's machine with the user's network identity (04 §yt-dlp).
//
//   yt-dlp   the extractor. Sites change their obfuscation weekly and it answers within
//            days, so it ships on its own cadence rather than Vortex's.
//   qjs      the JavaScript engine yt-dlp borrows for YouTube's player challenge. Without
//            one the extraction is on a deprecated path that quietly drops formats.
//   ffmpeg   the muxer. Every adaptive stream arrives as separate video and audio, so
//            "no ffmpeg" and "streaming does not work" are the same sentence.
await fetchYtDlp({ target });
await fetchQjs({ target });

// macOS is the one platform with no ffmpeg build this script will vouch for, and
// `tauri.macos.conf.json` leaves it out of `externalBin` to match. There it stays what it
// was before any of this: found on `PATH`, or reported missing in the user's own words.
if (target.includes("darwin") || target.includes("apple")) {
  console.log("ffmpeg — skipped: no verifiable macOS build; the bundle leaves it to PATH");
} else {
  await fetchFfmpeg({ target });
}

// The CLI's own entry point, run by this Node rather than through `npx`. npm's Windows
// shims are batch files, and since Node 24 those cannot be spawned without `shell: true` —
// which would put every argument back through a command-line parser for nothing.
const tauri = createRequire(import.meta.url).resolve("@tauri-apps/cli/tauri.js");
run(
  process.execPath,
  [tauri, "build", "--target", target, ...passthrough],
  join(ROOT, "apps", "desktop"),
);
