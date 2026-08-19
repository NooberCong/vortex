import type { Palette } from "./palette";
import { CHANNELS, type Lease } from "./segments";

/**
 * The segment map, drawn (05 §Signature §Rendering).
 *
 * Kept out of the component because what is worth being sure of here is a set of claims
 * about draw calls rather than about pixels: a column that is a seventh covered is filled
 * at a seventh alpha and never at one, and a run three hundred columns wide costs one
 * `fillRect` rather than three hundred. The first is the map's honesty; the second is its
 * frame rate.
 *
 * There are three ways to draw it, because a frame carries three genuinely different
 * amounts of knowledge:
 *
 * | | |
 * |---|---|
 * | `bar` | No block map at all — a paused, queued or finished job. A proportional bar, and nothing implied beyond it. |
 * | `coverage` | The collapsed row. Many small runs, already coarsened by the daemon to "done or moving", aggregated per device pixel. |
 * | `lanes` | The expanded row. A handful of large contiguous leases, one band each, in the worker's own hue. |
 *
 * One renderer forced over all three would have to pretend the coarse frame knows who owns
 * what, or that the fine one has nothing to say about it.
 */

/** Peak-to-trough alpha delta of the in-flight shimmer, over 1.4 s (05 §Rendering). */
const SHIMMER = 0.08;
const SHIMMER_PERIOD = 1400;
/** How fast the shimmer's phase travels along the bar. Slow enough to read as direction. */
const SHIMMER_WAVELENGTH = 0.09;

/**
 * In-flight regions, relative to complete ones.
 *
 * The spec's own shorthand is `▓` complete, `▒` in flight, `░` not yet — three densities of
 * *one* ink. In the collapsed bar that ink is the neutral complete hue, because the daemon
 * coarsens a summary to "done or moving" before sending it and there is no lane left to
 * name. In an expanded band it is the lane's own hue, and the two densities are that
 * worker's past and present.
 */
const IN_FLIGHT = 0.6;

/**
 * A single vertical sheen laid over whatever was painted.
 *
 * The map is the one live readout in an otherwise flat interface, and a flat fill reads as
 * a swatch rather than as something lit. One gradient, composited `source-atop` so it
 * touches only painted pixels, is the difference between a coloured rectangle and a bar
 * with a surface. It costs one fill per draw and no state.
 */
const SHEEN_TOP = 0.13;
const SHEEN_BOTTOM = 0.1;

/** Light bleed around an in-flight region in an expanded lane. Only there, and only faint. */
const GLOW = 0.55;

/** Alpha quantisation for coalescing. Finer than the eye, coarser than float noise. */
const STEP = 1 / 64;

interface Common {
  /** Device pixels. */
  width: number;
  height: number;
  palette: Palette;
  radius: number;
}

export type MapView = Common &
  (
    | {
        kind: "bar";
        /** Fraction complete, in `[0, 1]`. */
        done: number;
      }
    | {
        kind: "coverage";
        /** `width × CHANNELS`, from `segments.coverage`. */
        cov: Float32Array;
      }
    | {
        kind: "lanes";
        leases: readonly Lease[];
        blocks: number;
        /**
         * Per-band shatter progress in `[0, 1]`, same order as `leases`. All ones is the
         * settled state; the animation is the component's business, the geometry is here.
         */
        shatter: readonly number[];
        /** `performance.now()`, or `null` for no shimmer at all. */
        now: number | null;
      }
  );

export function paint(ctx: CanvasRenderingContext2D, view: MapView): void {
  const { width, height, palette } = view;
  ctx.clearRect(0, 0, width, height);
  if (width <= 0 || height <= 0) return;

  ctx.save();
  ctx.beginPath();
  ctx.roundRect(0, 0, width, height, view.radius);
  ctx.clip();

  // "Not yet" is a filled track rather than nothing, so an empty job is still a bar and a
  // job at 3% does not read as an accident.
  ctx.fillStyle = palette.track;
  ctx.fillRect(0, 0, width, height);

  if (view.kind === "bar") {
    ctx.fillStyle = palette.channels[0]!;
    ctx.fillRect(0, 0, width * clamp(view.done), height);
  } else if (view.kind === "coverage") {
    for (let channel = 0; channel < CHANNELS; channel += 1) {
      coverageBand(ctx, view, channel, channel === 0 ? 1 : IN_FLIGHT);
    }
  } else {
    laneBands(ctx, view);
  }

  sheen(ctx, width, height);
  ctx.restore();
}

let cached: { height: number; gradient: CanvasGradient } | null = null;

function sheen(ctx: CanvasRenderingContext2D, width: number, height: number): void {
  if (cached?.height !== height) {
    const gradient = ctx.createLinearGradient(0, 0, 0, height);
    gradient.addColorStop(0, `rgba(255,255,255,${SHEEN_TOP})`);
    gradient.addColorStop(0.5, "rgba(255,255,255,0)");
    gradient.addColorStop(1, `rgba(0,0,0,${SHEEN_BOTTOM})`);
    cached = { height, gradient };
  }
  ctx.globalCompositeOperation = "source-atop";
  ctx.fillStyle = cached.gradient;
  ctx.fillRect(0, 0, width, height);
  ctx.globalCompositeOperation = "source-over";
}

/**
 * Where band `index` sits at shatter progress `p`.
 *
 * At `p = 0` every band occupies the whole bar; at `p = 1` they are stacked. Running the
 * animation through the geometry rather than through opacity is what makes it read as one
 * bar splitting rather than as several bars fading in — the file *shatters into workers*,
 * which is the product's core mechanism made visible for a fifth of a second.
 */
export function band(
  index: number,
  bands: number,
  height: number,
  p: number,
): { y: number; h: number } {
  const settledHeight = height / bands;
  return {
    y: index * settledHeight * p,
    h: height + (settledHeight - height) * p,
  };
}

function laneBands(
  ctx: CanvasRenderingContext2D,
  view: Extract<MapView, { kind: "lanes" }>,
): void {
  const { leases, blocks, width, height, now } = view;
  if (blocks <= 0 || leases.length === 0) return;
  const scale = width / blocks;

  leases.forEach((lease, index) => {
    const { y, h } = band(index, leases.length, height, view.shatter[index] ?? 1);
    if (h <= 0) return;
    ctx.fillStyle = view.palette.channels[lease.lane] ?? view.palette.channels[0]!;

    // Fractional coordinates on purpose: a lease narrower than a device pixel is
    // antialiased by its own coverage, which is the same promise the coarse path keeps by
    // arithmetic — a partial block is never drawn as a whole one.
    const done = (lease.cursor - lease.from) * scale;
    if (done > 0) {
      ctx.globalAlpha = 1;
      ctx.fillRect(lease.from * scale, y, done, h);
    }
    const moving = (lease.to - lease.cursor) * scale;
    if (moving > 0) {
      ctx.globalAlpha = now === null ? IN_FLIGHT : IN_FLIGHT * wave(now, lease.cursor * scale);
      // The only place in the app with a glow, and it is around the bytes actually moving.
      ctx.shadowColor = ctx.fillStyle as string;
      ctx.shadowBlur = h * GLOW;
      ctx.fillRect(lease.cursor * scale, y, moving, h);
      ctx.shadowBlur = 0;
    }
  });
  ctx.globalAlpha = 1;
}

function coverageBand(
  ctx: CanvasRenderingContext2D,
  view: Extract<MapView, { kind: "coverage" }>,
  channel: number,
  scale: number,
): void {
  const { cov, width, height } = view;
  ctx.fillStyle = view.palette.channels[0]!;

  // One pass, emitting a rect only when the alpha changes. Adjacent columns inside a lease
  // are identical, which is why forty runs cost roughly a hundred fills instead of a
  // thousand.
  let runStart = -1;
  let runAlpha = 0;
  for (let column = 0; column <= width; column += 1) {
    const raw = column < width ? cov[column * CHANNELS + channel]! * scale : 0;
    const quantised = Math.round(raw / STEP) * STEP;
    if (quantised === runAlpha) continue;
    if (runStart >= 0 && runAlpha > 0) {
      ctx.globalAlpha = Math.min(1, runAlpha);
      ctx.fillRect(runStart, 0, column - runStart, height);
    }
    runStart = column;
    runAlpha = quantised;
  }
  ctx.globalAlpha = 1;
}

/**
 * The shimmer: a slow travelling wave over in-flight regions.
 *
 * Multiplicative rather than additive so a fully covered region stays inside `[0, 1]` — an
 * animation must never be able to push the map above the coverage it is describing.
 */
function wave(now: number, x: number): number {
  const phase = (now / SHIMMER_PERIOD) * Math.PI * 2 - x * SHIMMER_WAVELENGTH;
  return 1 - SHIMMER / 2 + (SHIMMER / 2) * Math.sin(phase);
}

/**
 * The shatter's easing — `cubic-bezier(.2, 0, 0, 1)`, the app's enter curve (05 §Motion).
 *
 * Solved by bisection rather than by a lookup table: it runs a handful of times per frame
 * during a 220 ms animation, and a table would be more code than the thing it saves.
 */
export function ease(t: number): number {
  const clamped = clamp(t);
  // Exact at the ends. Bisection lands within 1e-12 of them, which is invisible on screen
  // but leaves a band a fraction of a pixel short of where it belongs at rest.
  if (clamped <= 0 || clamped >= 1) return clamped;
  let lo = 0;
  let hi = 1;
  for (let i = 0; i < 20; i += 1) {
    const mid = (lo + hi) / 2;
    if (bezier(mid, 0.2, 0) < clamped) lo = mid;
    else hi = mid;
  }
  return bezier((lo + hi) / 2, 0, 1);
}

function bezier(t: number, a: number, b: number): number {
  const inv = 1 - t;
  return 3 * inv * inv * t * a + 3 * inv * t * t * b + t * t * t;
}

function clamp(v: number): number {
  return Math.min(1, Math.max(0, v));
}
