<script lang="ts">
  import type { Category } from "@vortex/proto";

  import { queue } from "$lib/store.svelte";

  /**
   * Filter by kind, above the list it filters (05 §Layout).
   *
   * This was a 200 px column down the left of the window, and the column was mostly empty:
   * a handful of short words and a Settings link, holding a fifth of the width away from
   * the only thing anybody opens this app to look at. Laid across the top it costs 36 px of
   * height, sits next to the search box that does the other half of the same job, and
   * leaves the window as one screen rather than two panes.
   *
   * Which chips there are is `queue.kinds`, and why is written there — it is a rule with a
   * case in it worth a test, not a line of markup.
   *
   * This is not there at all when there is nothing downloaded. An empty state is a
   * sentence inviting somebody to start; a bar of zeroes over it is the app talking about
   * itself first.
   */

  const filter = $derived(queue.filter);
  const mine = (kind: Category) => filter.by === "category" && filter.category === kind;

  let bar: HTMLElement | null = $state(null);

  /**
   * Where the selected chip's surface is, so it can be drawn once and moved.
   *
   * Giving every chip its own background and switching which one is lit means the
   * selection teleports: it is in one place on one frame and somewhere else on the next,
   * and the eye has to find it again each time. One surface that travels is the same
   * information with the trip included, and it is the difference between a control that
   * responds and a control that feels *connected* to the pointer.
   *
   * Measured rather than computed, because the chips are text and their widths are the
   * font's business. `null` before the first measurement, which is what stops the pill
   * sliding in from the left edge on the frame the bar appears.
   */
  let pill = $state<{ x: number; w: number } | null>(null);

  function measure(): void {
    if (!bar) return;
    const current = bar.querySelector<HTMLElement>('[aria-current="true"]');
    pill = current ? { x: current.offsetLeft, w: current.offsetWidth } : null;
  }

  $effect(() => {
    // Both of these move the pill: the selection, and which chips exist to the left of it.
    void queue.filter;
    void queue.kinds;
    measure();
  });

  /**
   * Chips are text, and text is not the same width on every machine. A web font arriving
   * one frame after the bar is drawn moves every chip after the first, and a pill measured
   * before that lands a few pixels out and stays there — there is no second selection to
   * correct it.
   */
  $effect(() => {
    if (!bar) return;
    const watch = new ResizeObserver(() => measure());
    for (const chip of bar.querySelectorAll(".chip")) watch.observe(chip);
    return () => watch.disconnect();
  });
</script>

{#if queue.jobs.length > 0}
  <nav class="filters scroll" aria-label="Filter by kind" bind:this={bar}>
    {#if pill}
      <div
        class="pill"
        aria-hidden="true"
        style:transform="translateX({pill.x}px)"
        style:width="{pill.w}px"
      ></div>
    {/if}

    <button
      class="chip"
      aria-current={filter.by === "all"}
      onclick={() => (queue.filter = { by: "all" })}
    >
      <span class="label" data-text="All">All</span>
      <span class="count num">{queue.jobs.length}</span>
    </button>

    {#each queue.kinds as kind (kind)}
      <button
        class="chip"
        aria-current={mine(kind)}
        onclick={() => (queue.filter = { by: "category", category: kind })}
      >
        <span class="label" data-text={kind}>{kind}</span>
        <span class="count num">{queue.byCategory.get(kind) ?? 0}</span>
      </button>
    {/each}
  </nav>
{/if}

<style>
  /*
   * On `--bg`, so a selected chip is paper raised off it — the same figure/ground the
   * column used, turned on its side. The list below is `--surface`, which puts a natural
   * edge under the bar without a second rule drawn across the window.
   */
  .filters {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s1);
    flex: none;
    /* `--s3`, not `--s4`: a chip's own `--s3` of padding sits inside it, so its *label*
       starts at `--s5` — the same column the filenames and the section headings start on.
       The chip's surface hangs one step left of the text, which is what a selected pill is
       supposed to do. */
    padding: var(--s2) var(--s3);
    /* Seven kinds and a narrow window: scrolls rather than wraps, because a bar that
       becomes two lines moves the whole list down. */
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
    user-select: none;
  }

  /*
   * The selection itself, drawn once. `transform` and `width` rather than `left`, so the
   * trip is on the compositor and the bar is not laying itself out sixty times a second
   * while it happens.
   *
   * It sits at the chips' own offset — `position: relative` on the bar above is what makes
   * `offsetLeft` mean that — so it scrolls with them rather than staying put when the bar
   * is scrolled sideways.
   */
  .pill {
    position: absolute;
    top: var(--s2);
    left: 0;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--surface);
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.06);
    transition:
      transform var(--calm) var(--ease),
      width var(--calm) var(--ease);
  }

  .chip {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex: none;
    height: 28px;
    padding: 0 var(--s3);
    border-radius: var(--radius-sm);
    color: var(--text-dim);
    white-space: nowrap;
    transition:
      background var(--quick) var(--ease),
      color var(--quick) var(--ease);
  }

  .chip:hover {
    background: color-mix(in oklab, var(--text) 5%, transparent);
    color: var(--text);
  }

  .chip[aria-current="true"]:hover {
    background: transparent;
  }

  /*
   * Selection is weight and surface, never colour — there is no accent to reach for, and
   * that is the constraint doing its job (05 §The one rule). The surface is the pill
   * above; this is the weight.
   */
  .chip[aria-current="true"] {
    color: var(--text);
    font-weight: 500;
  }

  /*
   * The bold copy of the label, laid out and never drawn.
   *
   * Without it every chip is one or two pixels narrower until it is selected, so choosing
   * one nudges every chip to its right — and the pill, which is chasing a target that
   * moved while it was on its way. Reserving the wider of the two widths costs nothing and
   * makes the bar geometrically still.
   */
  .label::after {
    content: attr(data-text);
    display: block;
    height: 0;
    overflow: hidden;
    font-weight: 500;
    visibility: hidden;
  }

  .count {
    font-size: var(--t-meta);
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }

  .chip[aria-current="true"] .count {
    color: var(--text-dim);
  }
</style>
