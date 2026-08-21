<script lang="ts">
  import { ease, paint, type MapView } from "$lib/paint";
  import { palette, reducedMotion } from "$lib/palette";
  import { buffer, coverage, leases, type Run } from "$lib/segments";
  import { onFrame } from "$lib/ticker";

  /**
   * The byte-range occupancy of a file, live (05 §Signature).
   *
   * Every competitor draws a progress bar. This draws where the bytes actually are — which
   * connections are live, whether one is stalling, that a resumed download really did keep
   * what it had, and the moment the scheduler takes work from a slow worker and gives it to
   * a fast one. It gets the app's only real animation and all of its colour, and everything
   * around it stays silent.
   *
   * The component owns the canvas, the observers and the clock. What to draw is `$lib/paint`
   * and `$lib/segments`, both pure and both tested — including the rule the whole thing
   * rests on, that a partial block is never drawn as a whole one.
   */

  interface Props {
    runs: readonly Run[];
    blocks: number;
    /** CSS pixels for a collapsed bar. A lane map sizes itself to fit its bands. */
    height?: number;
    /** Draw one band per worker instead of a single bar. */
    perLane?: boolean;
    /**
     * Fraction complete, used when there is no live frame. A paused or finished job has no
     * block map — the daemon only sends one while a job runs — and the bar still has to
     * agree with the percentage beside it.
     */
    ratio?: number | null;
    /** Spoken description, since colour is never the sole carrier of meaning. */
    description?: string;
  }

  const {
    runs,
    blocks,
    height = 6,
    perLane = false,
    ratio: fraction = null,
    description,
  }: Props = $props();

  /** Enough that a band is a band and not a hairline. Four lanes lands on the spec's 28 px. */
  const BAND = 7;
  const SHATTER_MS = 220;
  const STAGGER_MS = 20;

  const bands = $derived(perLane ? leases(runs) : []);
  const tall = $derived(perLane ? Math.max(height, bands.length * BAND) : height);

  let canvas: HTMLCanvasElement | null = $state(null);
  let context: CanvasRenderingContext2D | null = null;
  let cov = buffer(0);
  let width = 0;
  let dpr = 1;
  let visible = true;

  /**
   * When the current shatter began, or `null` once it has settled.
   *
   * It plays when a map gains its bands, which is when a row is expanded — so the split is
   * reproducible rather than missable, instead of happening once per download at a moment
   * nobody was watching.
   */
  let shatterFrom: number | null = null;
  let previousBands = 0;

  function measure(): boolean {
    if (!canvas) return false;
    dpr = window.devicePixelRatio || 1;
    const next = Math.round(canvas.getBoundingClientRect().width * dpr);
    const wanted = Math.round(tall * dpr);
    if (next !== width || canvas.height !== wanted) {
      width = next;
      canvas.width = width;
      canvas.height = wanted;
      cov = buffer(width);
    }
    return width > 0;
  }

  function view(now: number | null): MapView {
    const common = {
      width,
      height: canvas!.height,
      palette: palette(),
      radius: Math.min(3, (tall * dpr) / 2),
    };

    if (perLane && bands.length > 0 && blocks > 0) {
      const shatter =
        shatterFrom === null || now === null
          ? []
          : bands.map((_, i) => ease((now - shatterFrom! - i * STAGGER_MS) / SHATTER_MS));
      return { ...common, kind: "lanes", leases: bands, blocks, shatter, now };
    }

    if (runs.length > 0 && blocks > 0) {
      coverage(runs, blocks, width, cov);
      return { ...common, kind: "coverage", cov };
    }

    return { ...common, kind: "bar", done: fraction ?? 0 };
  }

  function draw(now: number | null): void {
    if (!canvas || !visible || !measure()) return;
    context ??= canvas.getContext("2d", { alpha: true });
    if (!context) return;

    paint(context, view(now));

    if (
      shatterFrom !== null &&
      now !== null &&
      now - shatterFrom > SHATTER_MS + bands.length * STAGGER_MS
    ) {
      shatterFrom = null;
    }
  }

  // Data. Redraws exactly when a frame arrives — 2 Hz collapsed, 20 Hz expanded — and not
  // otherwise, which is what takes an idle window to zero frames.
  $effect(() => {
    void runs;
    void blocks;
    void fraction;
    void tall;
    if (bands.length !== previousBands) {
      if (bands.length > 0 && previousBands === 0 && !reducedMotion()) {
        shatterFrom = performance.now();
      }
      previousBands = bands.length;
    }
    draw(shatterFrom === null ? null : performance.now());
  });

  /*
   * Motion, and only where there is motion worth drawing.
   *
   * The shimmer belongs to the row somebody opened. Forty 6 px bars breathing at once is
   * exactly the animation noise the shimmer exists to avoid, and a collapsed row already
   * moves twice a second, which is its whole budget.
   */
  $effect(() => {
    if (!visible || !perLane || reducedMotion() || !canvas) return;

    /*
     * The block this map lives in, if that block is one that folds away (`data-fold`, set
     * by whatever owns the transition). Resolved once, here, rather than per frame.
     *
     * Svelte pauses a block's effects *before* it plays the outro, so for the 180 ms a
     * fold takes, this component is on screen and its props are deriveds that nothing is
     * maintaining any more — `draw` reads them and Svelte says so in the console, which is
     * correct of it. Stopping is also just right: a map fading out has nothing left to
     * say, and those frames belong to the fold.
     *
     * It has to be an attribute rather than a prop because by then no prop can change:
     * inert effects do not re-run. `outrostart` is a DOM event on a DOM node, and DOM
     * nodes do not care whether Svelte has finished with them.
     */
    const block = canvas.closest("[data-fold]");
    let off: (() => void) | null = null;
    off = onFrame((now) => {
      if (block?.getAttribute("data-fold") === "leaving") off?.();
      else draw(now);
    });
    return () => off?.();
  });

  // Only visible rows render (05 §Rendering). A list of forty with six on screen costs six.
  $effect(() => {
    if (!canvas) return;
    const seen = new IntersectionObserver(
      ([entry]) => {
        visible = entry?.isIntersecting ?? true;
        if (visible) draw(null);
      },
      { rootMargin: "64px" },
    );
    const resized = new ResizeObserver(() => draw(null));
    seen.observe(canvas);
    resized.observe(canvas);
    return () => {
      seen.disconnect();
      resized.disconnect();
    };
  });
</script>

<!--
  The description sits on the wrapper rather than the canvas: colour is never the sole
  carrier of meaning (05 §Quality floor), and a canvas has no accessible content of its own
  to fall back to.
-->
<div class="map" role="img" aria-label={description ?? "Segment map"} style:height="{tall}px">
  <canvas bind:this={canvas}></canvas>
</div>

<style>
  .map {
    display: block;
    width: 100%;
    /* The map grows as workers arrive; the growth is a standard move, not a jump. */
    transition: height var(--calm) var(--ease);
  }

  canvas {
    display: block;
    width: 100%;
    height: 100%;
    border-radius: 3px;
  }
</style>
