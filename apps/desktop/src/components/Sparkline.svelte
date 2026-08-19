<script lang="ts">
  import { palette } from "$lib/palette";

  /**
   * Recent throughput, as bars.
   *
   * Two things use it: a worker's lane in the expanded row, and the aggregate trace in the
   * titlebar. Both answer the same question — is this steady, climbing or falling — which
   * a single smoothed number cannot, however honestly it is rounded.
   *
   * Bars rather than a line because the samples are discrete and few. A line between eight
   * points invents seven slopes that were never measured.
   */

  interface Props {
    /** Oldest first. Fewer than the full width draws right-aligned, so new samples enter from the right. */
    values: readonly number[];
    width?: number;
    height?: number;
    /** A channel index into the spectrum — 0 is the neutral "complete" hue. */
    channel?: number;
    /** Bars are drawn against this, so a quiet lane still reads as a lane. */
    faint?: boolean;
  }

  const { values, width = 64, height = 16, channel = 0, faint = false }: Props = $props();

  const BAR = 3;
  const GAP = 1;

  let canvas: HTMLCanvasElement | null = $state(null);

  $effect(() => {
    if (!canvas) return;
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);

    const slots = Math.floor((width + GAP) / (BAR + GAP));
    const recent = values.slice(-slots);
    // Scaled to the window's own peak, not to a global one: the question is the shape of
    // this lane over the last few seconds, and a lane that is slow throughout still has a
    // shape worth seeing.
    const peak = Math.max(1, ...recent);

    ctx.fillStyle = palette().channels[channel] ?? palette().channels[0]!;
    ctx.globalAlpha = faint ? 0.45 : 1;
    recent.forEach((value, i) => {
      // A minimum of one pixel: a sample of zero is a sample, and an absent bar reads as
      // missing data rather than as a stall.
      const bar = Math.max(1, Math.round((value / peak) * height));
      const x = width - (recent.length - i) * (BAR + GAP);
      ctx.fillRect(x, height - bar, BAR, bar);
    });
    ctx.globalAlpha = 1;
  });
</script>

<canvas
  bind:this={canvas}
  style:width="{width}px"
  style:height="{height}px"
  aria-hidden="true"
></canvas>

<style>
  canvas {
    display: block;
  }
</style>
