<script lang="ts">
  /**
   * The icon set, entire.
   *
   * Eleven glyphs on one 16-unit grid, stroked in `currentColor` at 1.5 — which is why
   * they are here rather than in an icon package: a set this small that all has to match
   * the type is cheaper to draw than to curate, and it cannot drift in weight when a
   * dependency updates.
   *
   * Nothing here is ever coloured. Colour is reserved for data (05 §The one rule).
   */

  export type Glyph =
    | "pause" | "play" | "close" | "folder" | "reveal" | "plus" | "settings"
    | "chevron" | "search" | "minimise" | "maximise";

  interface Props {
    name: Glyph;
    size?: number;
  }

  const { name, size = 14 }: Props = $props();

  const PATHS: Record<Glyph, string> = {
    pause: "M6 3.5v9M10 3.5v9",
    play: "M5 3.2l7 4.8-7 4.8z",
    close: "M4 4l8 8M12 4l-8 8",
    folder: "M2 4.5h4l1.2 1.6H14v6.4H2z",
    reveal: "M9 3h4v4M13 3L8 8M12.5 9.5v3.5H3V3.5h3.5",
    plus: "M8 3.5v9M3.5 8h9",
    settings: "M8 5.6A2.4 2.4 0 108 10.4 2.4 2.4 0 008 5.6zM8 1.8v1.6M8 12.6v1.6M14.2 8h-1.6M3.4 8H1.8M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1M12.4 12.4l-1.1-1.1M4.7 4.7L3.6 3.6",
    chevron: "M5.5 6.5L8 9l2.5-2.5",
    search: "M7.2 2.6a4.6 4.6 0 100 9.2 4.6 4.6 0 000-9.2zM10.6 10.6L13.6 13.6",
    minimise: "M3.5 8h9",
    maximise: "M3.5 3.5h9v9h-9z",
  };

  const FILLED: Glyph[] = ["play"];
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill="none"
  stroke="currentColor"
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  <path d={PATHS[name]} fill={FILLED.includes(name) ? "currentColor" : "none"} />
</svg>

<style>
  svg {
    display: block;
    flex: none;
  }
</style>
