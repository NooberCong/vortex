import { LANES } from "./segments";

/**
 * The spectrum, resolved once.
 *
 * The segment map is drawn on a canvas, and a canvas cannot read a CSS custom property —
 * it needs `rgb(33, 180, 212)`, not `var(--w1)`. Resolving those through
 * `getComputedStyle` is a forced style recalculation, which is precisely the thing that
 * must not happen twenty times a second in front of forty rows.
 *
 * So it happens once, and again only when the answer could have changed: the user picked a
 * theme, or the OS did. Both are edges, and both are rare.
 */

export interface Palette {
  /** Index 0 is "complete"; 1..=LANES are the worker lanes, matching `segments.channel`. */
  channels: string[];
  track: string;
  attention: string;
  text: string;
  dim: string;
}

let cached: Palette | null = null;
let watching = false;

export function palette(): Palette {
  if (!cached) {
    watch();
    cached = read();
  }
  return cached;
}

/** Exposed for the theme control, which knows it has just changed the answer. */
export function invalidate(): void {
  cached = null;
}

function read(): Palette {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string, fallback: string) =>
    style.getPropertyValue(name).trim() || fallback;

  return {
    channels: [
      token("--data-complete", "#7a9aef"),
      ...Array.from({ length: LANES }, (_, i) => token(`--w${i + 1}`, "#7a9aef")),
    ],
    track: token("--track", "#e7eaee"),
    attention: token("--attention", "#e5484d"),
    text: token("--text", "#10141a"),
    dim: token("--text-dim", "#5c6673"),
  };
}

function watch(): void {
  if (watching) return;
  watching = true;
  // `data-theme` on the root element is the explicit choice; the media query is the
  // system one. A theme switch has to repaint every canvas in the window, and the canvases
  // find out by asking for the palette on their next frame.
  new MutationObserver(invalidate).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", invalidate);
}

/**
 * Whether the user has asked for less motion.
 *
 * Read here rather than in each component because it gates a subscription, not a style:
 * the shimmer and the shatter are frames that are never scheduled, not frames that are
 * scheduled and drawn identically.
 */
export function reducedMotion(): boolean {
  return (
    document.documentElement.dataset.motion === "reduced" ||
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}
