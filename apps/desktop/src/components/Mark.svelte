<script lang="ts">
  /**
   * The Vortex mark, in the corner of the titlebar.
   *
   * The icon itself — the same azure plate and the same white figure the taskbar shows, from
   * the same generator. Everything inside the `ring` sentinels below is written by
   * `scripts/logo.mjs` and checked by `mark.test.ts`, so edit the geometry there, never the
   * `d` here.
   *
   * **It is the one coloured thing in the window** (05 §The one rule), and it is a deliberate
   * exception rather than a gap in the rule. Colour in this app means bytes: the eight-worker
   * spectrum and the single attention red. The mark is not data and it borrows nothing from
   * the spectrum's vocabulary — it is a fixed hue on a fixed plate that never changes with
   * state, which is exactly why it cannot be misread as a reading. What it buys is that the
   * thing in the corner of the window is recognisably the thing on the taskbar.
   *
   * 22 px, not the 20 the rest of the titlebar is built on. The reason is beside `SMALL` in
   * the generator: the figure needs the plate's contrast to hold its strokes at this size,
   * and it needs those two pixels to stay a rosette rather than a blue square with something
   * in it.
   */

  interface Props {
    /**
     * Turns while the queue is running.
     *
     * The one piece of decoration in the app that is not decoration: eight arms pulled round
     * is what the mark is *of*, so a mark that turns exactly while eight workers are pulling
     * is the drawing saying what it always meant. Twelve seconds a revolution — slow enough
     * to read as a state rather than a spinner, and it is the corner of the window, not a
     * progress dialog.
     *
     * The plate stays still and the figure turns inside it. A rotating rounded square would
     * be a spinner, and the plate is the part that has to stay recognisable.
     *
     * It pauses rather than stops, so the figure holds the angle it reached instead of
     * snapping back to square every time the queue goes quiet.
     */
    spinning?: boolean;
    size?: number;
  }

  const { spinning = false, size = 22 }: Props = $props();
</script>

<svg
  class="mark"
  class:spinning
  width={size}
  height={size}
  viewBox="0 0 1024 1024"
  aria-hidden="true"
>
  <!-- ring -->
  <rect width="1024" height="1024" rx="228" fill="#206ae1" />
  <g class="figure">
    <path
      d="M512 512L512 316M512 512L650.6 373.4M512 512L708 512M512 512L650.6 650.6M512 512L512 708M512 512L373.4 650.6M512 512L316 512M512 512L373.4 373.4M512 372L552.7 280.6M512 372L471.3 280.6M611 413L704.4 377.2M611 413L646.8 319.6M652 512L743.4 552.7M652 512L743.4 471.3M611 611L646.8 704.4M611 611L704.4 646.8M512 652L471.3 743.4M512 652L552.7 743.4M413 611L319.6 646.8M413 611L377.2 704.4M372 512L280.6 471.3M372 512L280.6 552.7M413 413L377.2 319.6M413 413L319.6 377.2M508.9 179.3A104 104 0 0 1 628 185.5A104 104 0 0 1 662.9 299.6M745.1 274.5A104 104 0 0 1 824.9 363.2A104 104 0 0 1 768.9 468.5M844.7 508.9A104 104 0 0 1 838.5 628A104 104 0 0 1 724.4 662.9M749.5 745.1A104 104 0 0 1 660.8 824.9A104 104 0 0 1 555.5 768.9M515.1 844.7A104 104 0 0 1 396 838.5A104 104 0 0 1 361.1 724.4M278.9 749.5A104 104 0 0 1 199.1 660.8A104 104 0 0 1 255.1 555.5M179.3 515.1A104 104 0 0 1 185.5 396A104 104 0 0 1 299.6 361.1M274.5 278.9A104 104 0 0 1 363.2 199.1A104 104 0 0 1 468.5 255.1"
      fill="none"
      stroke="#ffffff"
      stroke-width="50"
      stroke-linecap="round"
    />
  </g>
  <!-- /ring -->
</svg>

<style>
  .mark {
    display: block;
    flex: none;
  }

  /*
   * `view-box`, so the origin is the centre of the 1024 grid the figure was drawn on rather
   * than the centre of whatever box the strokes happen to occupy.
   */
  .figure {
    transform-box: view-box;
    transform-origin: 512px 512px;
    /* Always attached, never restarted: `animation-play-state` is what makes stopping a
       pause rather than a rewind. Starting the animation on demand instead would snap the
       figure back to square every time the last download finished. */
    animation: turn 12s linear infinite;
    animation-play-state: paused;
  }

  .mark.spinning .figure {
    animation-play-state: running;
  }

  @keyframes turn {
    to {
      transform: rotate(360deg);
    }
  }

  /*
   * The blanket rule in `app.css` clamps every animation to 100 ms, which for a rotation
   * means one 360° flick rather than no motion at all. A loop has to be turned off by
   * name, not shortened.
   */
  @media (prefers-reduced-motion: reduce) {
    .figure {
      animation: none;
    }
  }

  :global(:root[data-motion="reduced"]) .figure {
    animation: none;
  }
</style>
