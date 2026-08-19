import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

// Vitest runs from the package root, and WXT's transform rewrites `import.meta.url` into
// something that is no longer a file URL.
const root = process.cwd();
const out = join(root, ".output", "chrome-mv3-store");

/**
 * The store build, checked against the policy it exists to satisfy.
 *
 * The Chrome Web Store forbids "enabling the unauthorized access, download, or streaming
 * of copyrighted content or media", and the 2025–2026 enforcement waves cleared out most
 * of the category (03 §Store strategy). So the store build ships generic HTTP capture and
 * nothing else, with the media pipeline living in `vortexd` where the store has no say.
 *
 * This is a test rather than a habit because the failure is silent and expensive: a
 * refactor that moves one import can put the manifest sniffer back into the bundle, the
 * build still succeeds, and the first sign of trouble is a takedown. A reviewer reads
 * files, so the assertion is about files.
 */
describe("the store build", () => {
  let bundle = "";
  let manifest: {
    permissions: string[];
    description: string;
    web_accessible_resources?: unknown;
    key?: string;
  };

  beforeAll(() => {
    execFileSync("npx", ["wxt", "build", "--mode", "store"], {
      cwd: root,
      stdio: "pipe",
      shell: process.platform === "win32",
    });
    bundle = files(out)
      .filter((path) => path.endsWith(".js"))
      .map((path) => readFileSync(path, "utf8"))
      .join("\n");
    manifest = JSON.parse(readFileSync(join(out, "manifest.json"), "utf8"));
  }, 120_000);

  afterAll(() => {
    // A store build regenerates `.wxt/types/paths.d.ts` without the MSE entrypoints, and
    // a later `tsc` would then reject the two references to them. Putting the default
    // types back is cheaper than making everyone downstream remember this.
    execFileSync("npx", ["wxt", "prepare"], {
      cwd: root,
      stdio: "pipe",
      shell: process.platform === "win32",
    });
  }, 120_000);

  it("contains no streaming vocabulary at all", () => {
    // Not "does not run" — does not exist. Tree-shaking on a build-time constant is what
    // makes that true, and this is the assertion that keeps it true.
    for (const term of [
      "m3u8",
      "mpegurl",
      "dash+xml",
      "probeMedia",
      "mediaFound",
      "MediaSource",
      "addSourceBuffer",
      "vortex-overlay",
      // Channel 5 hands a page URL to an extractor, which is the most streaming-shaped
      // thing in here and the last thing a store listing should contain.
      "orphan",
    ]) {
      expect(bundle.toLowerCase(), `"${term}" survived into the store build`).not.toContain(
        term.toLowerCase(),
      );
    }
  });

  it("ships no MAIN-world script", () => {
    // The two MSE entrypoints are roots of their own; nothing calls them in this build,
    // so nothing would tree-shake them. They are dropped from the entrypoint list instead.
    expect(readdirSync(out)).not.toContain("mse-hook.js");
    expect(readdirSync(out)).not.toContain("mse-loader.js");
    expect(manifest.web_accessible_resources).toBeUndefined();
  });

  it("asks for no permission it no longer uses", () => {
    // `scripting` exists only to register the MSE loader per origin. Asking for it in a
    // build that cannot use it is an unexplainable permission on a store listing.
    expect(manifest.permissions).not.toContain("scripting");
    // And the ones capture genuinely needs are still there.
    expect(manifest.permissions).toEqual(
      expect.arrayContaining(["webRequest", "downloads", "cookies", "nativeMessaging"]),
    );
    // Never blocking. Vortex observes and then cancels; asking to block would be both
    // unnecessary and, in MV3, restricted to policy-installed extensions.
    expect(manifest.permissions).not.toContain("webRequestBlocking");
  });

  it("claims no id of its own", () => {
    // Development builds carry `key` so their id is predictable enough to register against
    // (`identity.ts`). The listing's id belongs to the listing, and is added to
    // `vortex_setup::CHROMIUM_IDS` once the item exists.
    expect(manifest.key).toBeUndefined();
  });

  it("describes itself as a download manager and names no site", () => {
    expect(manifest.description.toLowerCase()).toContain("download manager");
    for (const site of ["youtube", "netflix", "video", "stream"]) {
      expect(manifest.description.toLowerCase()).not.toContain(site);
    }
  });
});

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}
