import type { TransitionConfig } from "svelte/transition";

/**
 * The app's motion, for the parts of it JavaScript has to drive.
 *
 * Most movement in this window is a CSS transition and belongs in the component that
 * moves — a hover, a press, a colour. This file is for the other kind: an element that has
 * to animate *out* before it is removed. CSS cannot do that, because by the time the rule
 * would apply the node is gone, so those are Svelte transitions and they need the curves
 * as functions rather than as `cubic-bezier(…)` strings.
 *
 * Two of them, matching 05 §Motion exactly:
 *
 *   enter  `cubic-bezier(.2, 0, 0, 1)`   leaves fast, arrives slowly — a thing settling
 *   exit   `cubic-bezier(.4, 0, 1, 1)`   leaves slowly, goes fast — a thing dismissed
 *
 * The asymmetry is the point. Something arriving should decelerate into place, because the
 * eye needs to find it; something leaving should accelerate away, because the eye is
 * already looking at what replaced it. A dialog that fades out on the same curve it faded
 * in on feels reluctant.
 *
 * The four numbers per curve are also in `tokens.css` as `--ease` and `--exit`, and
 * `motion.test.ts` fails if the two copies drift. That duplication is deliberate — a CSS
 * custom property cannot be called from JavaScript without a layout read per transition,
 * and a *slightly* different curve in two halves of one animation is the kind of wrongness
 * nobody can name and everybody can feel.
 *
 * No springs. No bounce. No overshoot. This is an instrument.
 */

/** `--ease`. */
export const ENTER = [0.2, 0, 0, 1] as const;
/** `--exit`. */
export const EXIT = [0.4, 0, 1, 1] as const;

/** `--quick`: hover, focus, press. */
export const QUICK = 120;
/** `--calm`: modal, row expand, list insert. */
export const CALM = 180;

/**
 * A CSS `cubic-bezier` as an easing function.
 *
 * The curve is parametric — both x and y are functions of some t that is not time — so
 * getting y for a given x means solving for t first. Newton–Raphson does it in a handful
 * of steps for any curve whose x is monotonic, which every legal `cubic-bezier` is, and
 * six is comfortably enough at the precision a 180 ms animation samples at.
 */
export function bezier([x1, y1, x2, y2]: readonly [number, number, number, number]) {
  // The cubic with P₀ = 0 and P₃ = 1, expanded to Horner form.
  const at = (t: number, a: number, b: number): number =>
    ((1 - 3 * b + 3 * a) * t + (3 * b - 6 * a)) * t * t + 3 * a * t;
  const slope = (t: number, a: number, b: number): number =>
    3 * (1 - 3 * b + 3 * a) * t * t + 2 * (3 * b - 6 * a) * t + 3 * a;

  return (x: number): number => {
    if (x <= 0) return 0;
    if (x >= 1) return 1;
    let t = x;
    for (let i = 0; i < 6; i++) {
      const d = slope(t, x1, x2);
      if (d === 0) break;
      t -= (at(t, x1, x2) - x) / d;
    }
    return at(t, y1, y2);
  };
}

export const enter = bezier(ENTER);
export const exit = bezier(EXIT);

interface Options {
  duration?: number;
  easing?: (t: number) => number;
}

/**
 * Whatever transform the element already carries, as something we can build on.
 *
 * Every centred surface in this app is centred with `translate(-50%, -50%)`, and a
 * transition that wrote `transform: scale(.98)` would throw that away and drop the panel
 * to the bottom-right corner for the length of the animation. The computed value is a
 * matrix with the percentages already resolved, so appending to it composes rather than
 * replaces.
 */
function carried(node: Element): string {
  const already = getComputedStyle(node).transform;
  return already === "none" ? "" : already;
}

/**
 * Grows into place, or shrinks out of it. The modal surface (05 §Screens).
 *
 * The 2% is deliberately almost nothing: enough that the panel arrives rather than
 * appears, not enough to read as a zoom.
 */
export function grow(
  node: Element,
  { duration = CALM, easing = enter, start = 0.98 }: Options & { start?: number } = {},
): TransitionConfig {
  const base = carried(node);
  const distance = 1 - start;
  return {
    duration,
    easing,
    css: (t) => `transform: ${base} scale(${1 - distance * (1 - t)}); opacity: ${t};`,
  };
}

/**
 * Comes up from below, goes back down. For a thing that lives at an edge of the window.
 */
export function rise(
  node: Element,
  { duration = CALM, easing = enter, y = 8 }: Options & { y?: number } = {},
): TransitionConfig {
  const base = carried(node);
  return {
    duration,
    easing,
    css: (t) => `transform: ${base} translateY(${(1 - t) * y}px); opacity: ${t};`,
  };
}

/**
 * Folds a block open, or folds it away — height, so what is beside it moves too.
 *
 * This is the one transition here that is not free. Animating height is layout every
 * frame, and layout every frame is exactly what the rest of the app is careful not to do.
 * It earns it: a row leaving the list has to take its space with it, and the rows below
 * closing the gap *continuously* is the difference between a list that reflows and a list
 * that settles. The alternative is a FLIP pass over every sibling, which is more code, and
 * more code running in the same frame.
 *
 * One row at a time, though — never forty. `queue.animating` is what enforces that; the
 * reason is written there.
 *
 * The opacity runs at twice the rate so the content is gone by the halfway point and what
 * is left is a gap closing. A row that is still legible while it is 12 px tall is a row
 * being squashed, which reads as a bug.
 */
export function fold(
  node: Element,
  { duration = CALM, easing = enter }: Options = {},
): TransitionConfig {
  // A zero duration means "this change is not worth announcing" — see `queue.animating`.
  // Returning a config with no `css` is not the same as a 0 ms animation: the browser is
  // never handed a keyframe at all, so there is no frame in which the row is 0 px tall.
  if (duration <= 0) return { duration: 0 };

  const style = getComputedStyle(node);
  // The *border* box, not `style.height`, which is the content box. Everything in this app
  // is `border-box`, so the number written back has to be the one the browser will read as
  // the whole thing — otherwise the element lands a padding short of where it started and
  // snaps the difference on the last frame.
  const height = node.getBoundingClientRect().height;
  const top = parseFloat(style.paddingTop) || 0;
  const bottom = parseFloat(style.paddingBottom) || 0;
  const border = parseFloat(style.borderBottomWidth) || 0;
  return {
    duration,
    easing,
    css: (t) =>
      `overflow: hidden;` +
      `height: ${t * height}px;` +
      `padding-top: ${t * top}px;` +
      `padding-bottom: ${t * bottom}px;` +
      `border-bottom-width: ${t * border}px;` +
      `opacity: ${Math.min(1, t * 2)};`,
  };
}

/**
 * Opacity, and nothing else.
 *
 * Svelte ships one of these; this one is here so that every transition in the app takes
 * the same two options and reads from the same pair of curves, rather than one import
 * quietly running on `linear` because that is what its default happened to be.
 */
export function fade(
  node: Element,
  { duration = QUICK, easing = enter }: Options = {},
): TransitionConfig {
  const target = Number(getComputedStyle(node).opacity);
  return { duration, easing, css: (t) => `opacity: ${t * target};` };
}
