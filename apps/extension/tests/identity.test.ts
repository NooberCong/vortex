import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import config from "../wxt.config";
import { EXTENSION_KEY, extensionId } from "../identity";

// Vitest runs from the package root.
const rust = readFileSync(
  join(process.cwd(), "..", "..", "crates", "vortex-setup", "src", "lib.rs"),
  "utf8",
);

/** `pub const CHROMIUM_IDS: &[&str] = &["…", "…"];`, as a list. */
function compiledInIds(): string[] {
  const block = rust.match(/CHROMIUM_IDS: &\[&str\] = &\[([\s\S]*?)\];/);
  if (!block?.[1]) throw new Error("vortex-setup no longer declares CHROMIUM_IDS this way");
  return [...block[1].matchAll(/"([a-p]{32})"/g)].map((m) => m[1]!);
}

/** WXT's manifest is a function of the target; call it the way the build does. */
function manifestFor(browser: "chrome" | "firefox", mode: string): Record<string, unknown> {
  const build = config.manifest;
  if (typeof build !== "function") throw new Error("wxt.config no longer builds the manifest");
  return build({
    browser,
    manifestVersion: browser === "firefox" ? 2 : 3,
    mode,
    command: "build",
  }) as Record<string, unknown>;
}

/**
 * One identity, three files.
 *
 * The key is declared in `identity.ts`, the id follows from it by arithmetic, and
 * `vortex_setup::CHROMIUM_IDS` has to carry that id or `vortexd --register` writes a
 * manifest the extension cannot match. Nothing about that failure is loud: the extension
 * goes passive and says nothing, which looks exactly like a crashed daemon (03 §2).
 *
 * So it is asserted rather than remembered.
 */
describe("the extension's identity", () => {
  it("is pinned by the key, not by where the folder happens to be", () => {
    expect(manifestFor("chrome", "production").key).toBe(EXTENSION_KEY);
  });

  it("is the id vortexd registers", () => {
    expect(compiledInIds()).toContain(extensionId());
  });

  it("is left to the store in the store build", () => {
    // The listing's id is assigned when the item is created and is not this one. Claiming
    // otherwise in the uploaded package is at best ignored.
    expect(manifestFor("chrome", "store").key).toBeUndefined();
  });

  it("is not something Firefox is told about", () => {
    // Gecko has no `key`; its identity is `browser_specific_settings.gecko.id`, which is
    // what `vortex_setup::GECKO_ID` pins.
    const firefox = manifestFor("firefox", "production");
    expect(firefox.key).toBeUndefined();
    expect(rust).toContain(
      `GECKO_ID: &str = "${
        (firefox.browser_specific_settings as { gecko: { id: string } }).gecko.id
      }"`,
    );
  });
});
