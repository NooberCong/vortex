import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { LANES, laneVar } from "../src/index";

const css = readFileSync(fileURLToPath(new URL("../tokens.css", import.meta.url)), "utf8");

/**
 * The three blocks that carry a palette: paper, dark-by-preference, dark-by-choice.
 *
 * A leaf ruleset is one whose body declares `--w1` and contains no nested block, which
 * skips the `@media` wrapper without needing a CSS parser to know it is one.
 */
function blocks(): string[] {
  const found: string[] = [];
  for (let open = 0; open < css.length; open += 1) {
    if (css[open] !== "{") continue;
    const close = css.indexOf("}", open);
    if (close < 0) break;
    const body = css.slice(open + 1, close);
    if (body.includes("--w1:") && !body.includes("{")) found.push(body);
  }
  return found;
}

const palettes = blocks();

describe("the data spectrum", () => {
  it("declares every lane in every theme", () => {
    // A lane declared in one theme and forgotten in another is a worker that vanishes when
    // the user switches to dark — the kind of bug nobody finds because nobody switches
    // themes while looking at a sixteen-connection download.
    expect(palettes.length).toBeGreaterThanOrEqual(3);
    for (const body of palettes) {
      const declared = [...body.matchAll(/--w([1-8]):\s*#[0-9a-f]{6};/g)].map((m) =>
        Number(m[1]),
      );
      expect(declared).toEqual(Array.from({ length: LANES }, (_, i) => i + 1));
    }
  });

  it("keeps the two dark blocks identical", () => {
    // CSS cannot OR a media query with a selector, so the dark palette is written twice:
    // once for `prefers-color-scheme` and once for an explicit choice. This is what makes
    // that duplication safe rather than a slow divergence.
    const dark = palettes.slice(1).map(normalise);
    expect(dark).toHaveLength(2);
    expect(dark[0]).toBe(dark[1]);
  });

  it("sweeps rather than repeats", () => {
    // Eight hues that need to be told apart at 2 px. Neighbouring lanes must actually
    // differ, and no two lanes anywhere may collide.
    for (const body of palettes) {
      const values = [...body.matchAll(/--w[1-8]:\s*(#[0-9a-f]{6});/g)].map((m) => m[1]!);
      expect(new Set(values).size, `duplicate lane colour in ${values.join(" ")}`).toBe(LANES);
    }
  });

  it("points `complete` and `focus` at a lane rather than at a loose colour", () => {
    // Both are aliases on purpose: a theme switch moves the whole family, and a hard-coded
    // hex here would be the one value that did not move with it.
    expect(/--data-complete:\s*var\(--w[1-8]\);/.test(css)).toBe(true);
    expect(/--focus:\s*var\(--w[1-8]\);/.test(css)).toBe(true);
  });

  it("wraps rather than running out on a fast origin", () => {
    // Twenty-four connections is an ordinary number for a CDN. Lane colour must stay a
    // total function; a worker with no colour is an invisible worker.
    expect(laneVar(1)).toBe("--w1");
    expect(laneVar(8)).toBe("--w8");
    expect(laneVar(9)).toBe("--w1");
    expect(laneVar(24)).toBe("--w8");
    expect(laneVar(0)).toBe("--w8");
  });
});

function normalise(body: string): string {
  return body.replace(/\s+/g, " ").trim();
}
