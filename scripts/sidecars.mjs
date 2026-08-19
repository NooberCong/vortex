/**
 * Stages `vortexd` and `vortex-host` where Tauri expects to find sidecars.
 *
 * Tauri wants each external binary suffixed with the target triple — `vortexd-x86_64-pc-
 * windows-msvc.exe` — and strips the suffix again when it bundles, so what lands beside
 * `Vortex.exe` on the user's machine is plain `vortexd.exe`. That is not incidental:
 * `vortex_ipc::start` resolves the daemon from beside the calling executable rather than
 * through `PATH`, and `vortex-setup` writes that same directory into every browser's
 * native-messaging manifest. One directory, three programs, no search path.
 *
 *   node scripts/sidecars.mjs [--target <triple>] [--debug]
 */

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/** The two programs that ship beside the window but are not the window. */
const SIDECARS = ["vortexd", "vortex-host"];

/**
 * The triple everything must agree on.
 *
 * `rustc -vV` is the only authority on what this machine's toolchain is called — and it
 * has to be asked, because the Tauri CLI otherwise assumes the triple *it* was built for.
 * On a Windows box using the GNU toolchain that guess is `-msvc`, and the failure is a
 * bundle that cannot find sidecars that are sitting right there under another name. So
 * `bundle.mjs` passes this to `tauri build` explicitly.
 */
export function hostTriple() {
  const version = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const host = version.match(/^host:\s*(\S+)$/m);
  if (!host) throw new Error("rustc -vV did not report a host triple");
  return host[1];
}

export function run(command, commandArgs, cwd = ROOT) {
  execFileSync(command, commandArgs, { cwd, stdio: "inherit" });
}

export function stage(target, debug = false) {
  const profile = debug ? "debug" : "release";
  run("cargo", [
    "build",
    ...(debug ? [] : ["--release"]),
    "--target",
    target,
    ...SIDECARS.flatMap((name) => ["-p", name]),
  ]);

  const out = join(ROOT, "apps", "desktop", "src-tauri", "binaries");
  mkdirSync(out, { recursive: true });
  const extension = target.includes("windows") ? ".exe" : "";
  for (const name of SIDECARS) {
    const built = join(ROOT, "target", target, profile, `${name}${extension}`);
    const staged = join(out, `${name}-${target}${extension}`);
    copyFileSync(built, staged);
    console.log(`${name} → ${staged}`);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const target = args.includes("--target") ? args[args.indexOf("--target") + 1] : hostTriple();
  stage(target, args.includes("--debug"));
}
