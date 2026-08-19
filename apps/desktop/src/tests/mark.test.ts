import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { figure, MARK, parts } from "../../../../scripts/logo.mjs";

const root = process.cwd();
const html = readFileSync(join(root, "index.html"), "utf8");
const icon = readFileSync(join(root, "src-tauri", "icons", "vortex.svg"), "utf8");
const tokens = readFileSync(join(root, "..", "..", "packages", "tokens", "tokens.css"), "utf8");

/**
 * The mark exists in two files and is generated into both. These are the assertions that
 * make that safe.
 *
 * The failure they catch is not hypothetical: `index.html` is hand-written apart from one
 * block, so the obvious thing to do when the splash needs a tweak is to edit the paths in
 * place — and then the icon on the taskbar and the figure on the boot screen are two
 * different drawings, which nobody notices because nobody sees them side by side.
 *
 * `npm run logo` regenerates both.
 */
describe("the mark", () => {
  it("is the same drawing in the splash and in the icon", () => {
    const { spokes, branches, curls } = parts();
    for (const [name, group] of [
      ["spokes", spokes],
      ["branches", branches],
      ["curls", curls],
    ] as const) {
      expect(html, `the splash's ${name} are stale — run \`npm run logo\``).toContain(
        `class="mark-${name}" pathLength="1" d="${group.join("")}"`,
      );
    }
    expect(icon, "the icon is stale — run `npm run logo`").toContain(`d="${figure()}"`);
  });

  it("is stroked at one weight in both", () => {
    expect(html).toContain(`stroke-width="${MARK.w}"`);
    expect(icon).toContain(`stroke-width="${MARK.w}"`);
  });

  it("keeps the icon's XML comment free of the one sequence XML forbids", () => {
    // `--` inside a comment makes the file unparseable, and the SVG rasteriser behind
    // `tauri icon` panics rather than reporting it. Cheap to assert, expensive to rediscover.
    const comment = icon.slice(icon.indexOf("<!--") + 4, icon.indexOf("-->"));
    expect(comment).not.toContain("--");
  });
});

/**
 * The splash paints before the bundle, so it cannot read the tokens file; it carries its
 * own copies of four values. This is what stops those copies from quietly becoming wrong.
 */
describe("the splash palette", () => {
  /** The body of one rule, so a token is read from the theme it belongs to. */
  const rule = (selector: RegExp): string => {
    const found = tokens.match(new RegExp(`${selector.source}\\s*\\{([\\s\\S]*?)\\n\\}`));
    if (!found?.[1]) throw new Error(`tokens.css has no rule matching ${selector}`);
    return found[1];
  };

  const value = (name: string, body: string): string => {
    const found = body.match(new RegExp(`${name}:\\s*(#[0-9a-f]{6})`, "i"));
    if (!found?.[1]) throw new Error(`no ${name} in that rule`);
    return found[1];
  };

  it("matches tokens.css", () => {
    const themes = [
      {
        // Skips the header comment, which also mentions the selector.
        body: rule(/\n:root,\n:host/),
        want: { "--bg": "#eef0f5", "--w4": "#206ae1", "--text-dim": "#4e5768" },
      },
      {
        body: rule(/\n:root\[data-theme="dark"\],\n:host\(\[data-theme="dark"\]\)/),
        want: { "--bg": "#0a0c11", "--w4": "#5796fe", "--text-dim": "#9aa5b8" },
      },
    ];

    for (const { body, want } of themes) {
      for (const [token, hex] of Object.entries(want)) {
        expect(value(token, body), token).toBe(hex);
        expect(html, `the splash has no ${hex}`).toContain(hex);
      }
    }
  });
});
