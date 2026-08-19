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
import { fetchYtDlp } from "./ytdlp.mjs";

const passthrough = process.argv.slice(2);
const target = hostTriple();

stage(target);
// The third sidecar, which cargo does not build. Extractors break weekly and answer within
// days, so yt-dlp is fetched at build time against the checksum its own release publishes
// rather than vendored into the repository (04 §yt-dlp as the extractor fallback).
await fetchYtDlp({ target });

// The CLI's own entry point, run by this Node rather than through `npx`. npm's Windows
// shims are batch files, and since Node 24 those cannot be spawned without `shell: true` —
// which would put every argument back through a command-line parser for nothing.
const tauri = createRequire(import.meta.url).resolve("@tauri-apps/cli/tauri.js");
run(
  process.execPath,
  [tauri, "build", "--target", target, ...passthrough],
  join(ROOT, "apps", "desktop"),
);
