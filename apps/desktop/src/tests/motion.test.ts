import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { bezier, CALM, enter, ENTER, exit, EXIT, fold, QUICK } from "$lib/motion";

const tokens = readFileSync(
  join(process.cwd(), "..", "..", "packages", "tokens", "tokens.css"),
  "utf8",
);

/**
 * `motion.ts` carries a second copy of the two curves and the two durations, because a CSS
 * custom property is not callable from JavaScript. These are the assertions that keep the
 * copy honest.
 *
 * The failure is a quiet one and that is exactly why it is worth a test: nothing breaks
 * when the curves drift apart. The panel fades on one curve and the scrim behind it fades
 * on another, three hundredths of a second apart, and the result is a window that feels
 * slightly cheap for a reason nobody can point at.
 */
describe("the curves in two places", () => {
  const value = (name: string): string => {
    const found = tokens.match(new RegExp(`${name}:\\s*([^;]+);`));
    if (!found?.[1]) throw new Error(`tokens.css has no ${name}`);
    return found[1].trim();
  };

  it("is the same enter curve in CSS and in JavaScript", () => {
    expect(value("--ease")).toBe(`cubic-bezier(${ENTER.join(", ")})`);
  });

  it("is the same exit curve in CSS and in JavaScript", () => {
    expect(value("--exit")).toBe(`cubic-bezier(${EXIT.join(", ")})`);
  });

  it("is the same two durations", () => {
    expect(value("--quick")).toBe(`${QUICK}ms`);
    expect(value("--calm")).toBe(`${CALM}ms`);
  });
});

describe("the bezier solver", () => {
  it("is the identity for the linear curve", () => {
    // The one case with a closed form to check against: control points on the diagonal
    // make x and y the same function of t, so the solver must give back what it was given.
    const linear = bezier([0, 0, 1, 1]);
    for (const x of [0.1, 0.25, 0.5, 0.75, 0.9]) expect(linear(x)).toBeCloseTo(x, 6);
  });

  it("pins both ends", () => {
    for (const curve of [enter, exit]) {
      expect(curve(0)).toBe(0);
      expect(curve(1)).toBe(1);
      // Out of range is what a transition asks for when it is interrupted mid-flight.
      expect(curve(-0.5)).toBe(0);
      expect(curve(1.5)).toBe(1);
    }
  });

  it("never goes backwards, and never overshoots", () => {
    // 05 §Motion: no springs, no bounce. A curve that left [0, 1] would be a bounce, and a
    // curve that dipped would be a stutter — this is the shape of that rule as an assertion.
    for (const curve of [enter, exit]) {
      let last = 0;
      for (let i = 0; i <= 100; i++) {
        const y = curve(i / 100);
        expect(y).toBeGreaterThanOrEqual(last - 1e-9);
        expect(y).toBeLessThanOrEqual(1);
        last = y;
      }
    }
  });

  it("arrives slowly and leaves quickly, which is the whole point of having two", () => {
    // Enter is front-loaded: most of the distance is covered early, so the thing decelerates
    // into place. Exit is the reverse. If these ever swapped, everything would still animate
    // and everything would feel wrong.
    expect(enter(0.5)).toBeGreaterThan(0.75);
    expect(exit(0.5)).toBeLessThan(0.35);
  });
});

describe("fold", () => {
  const block = (): HTMLElement => {
    const node = document.createElement("div");
    document.body.append(node);
    return node;
  };

  it("does nothing at all when the change is not worth announcing", () => {
    // Not "a 0 ms animation" — no `css` at all, so the browser is never handed a keyframe
    // and there is no frame in which the row is zero pixels tall. See `queue.animating`.
    expect(fold(block(), { duration: 0 }).css).toBeUndefined();
  });

  it("ends where it started, so nothing snaps on the last frame", () => {
    const node = block();
    const at = fold(node, { duration: CALM }).css!(1, 0);
    expect(at).toContain("opacity: 1");
    // jsdom reports no layout, so the interesting number here is the shape, not the value:
    // whatever the height was, t = 1 has to write back exactly that and not a fraction.
    expect(at).toMatch(/height: 0px|height: \d/);
    expect(fold(node, { duration: CALM }).css!(0, 1)).toContain("height: 0px");
  });

  it("clears the content before the gap has finished closing", () => {
    // Halfway through, the row is half as tall and already invisible. A row that is still
    // legible at 12 px is a row being squashed, which reads as a bug rather than as motion.
    expect(fold(block(), { duration: CALM }).css!(0.5, 0.5)).toContain("opacity: 1");
    expect(fold(block(), { duration: CALM }).css!(0.25, 0.75)).toContain("opacity: 0.5");
  });
});
