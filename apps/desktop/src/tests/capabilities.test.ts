import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

// Vitest runs from the package root.
const capability = JSON.parse(
  readFileSync(join(process.cwd(), "src-tauri", "capabilities", "default.json"), "utf8"),
) as { permissions: Array<string | { identifier: string; allow?: Array<{ path?: string }> }> };

function entry(identifier: string) {
  return capability.permissions.find(
    (p) => p === identifier || (typeof p === "object" && p.identifier === identifier),
  );
}

/**
 * The Open button's permission, and the shape it has to have.
 *
 * `opener:allow-open-path` enables the command and nothing else. The command ends in
 * `is_path_allowed`, which is `fs_scope.is_allowed(path) && self.allowed.iter().any(..)` —
 * so an empty allow-list makes the second half `false` for every path on the machine and
 * the plugin answers `ForbiddenPath`. The bare permission is a button that has never once
 * worked, and the failure reads as "that file isn't there any more" to whoever wrote the
 * catch, which is the worst possible sentence: it is a confident claim about the user's
 * disk that sends them to look for a file that is sitting in the folder.
 *
 * Nothing else catches this. It compiles, it starts, the row renders, the button is
 * clickable, and "Show in folder" beside it keeps working — `reveal_item_in_dir` takes no
 * scope at all — so the one broken thing looks like a broken file rather than a broken
 * permission.
 */
describe("the opener capability", () => {
  it("gives open-path a scope to match against", () => {
    const open = entry("opener:allow-open-path");
    expect(open, "opener:allow-open-path is missing; the Open button cannot work").toBeDefined();
    expect(
      typeof open === "object" && open.allow?.length,
      "opener:allow-open-path has no allow-list, so it denies every path",
    ).toBeTruthy();
  });

  it("allows the destinations a download can actually land in", () => {
    const open = entry("opener:allow-open-path");
    const paths = typeof open === "object" ? (open.allow ?? []).map((a) => a.path) : [];
    // The destination is the user's to choose and routinely is not under the home
    // directory - large video goes on the spare volume - so an enumeration of `$DOWNLOAD`,
    // `$VIDEO` and friends would be a list of the places we guessed they keep their files.
    expect(paths).toContain("**");
  });

  it("still asks for reveal, which is the fallback when opening fails", () => {
    expect(entry("opener:allow-reveal-item-in-dir")).toBe("opener:allow-reveal-item-in-dir");
  });
});
