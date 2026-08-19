<script lang="ts">
  import { queue } from "$lib/store.svelte";
  import EmptyState from "./EmptyState.svelte";
  import JobRow from "./JobRow.svelte";

  /**
   * The list.
   *
   * Not virtualised, deliberately. The quality floor is 60 fps with forty jobs listed
   * (05 §Quality floor), and forty rows of three lines each is nothing for a DOM — the
   * frame budget goes on the canvases, which are gated by `IntersectionObserver` and
   * already only draw when they are on screen. Virtualising would add a scroll-position
   * bug surface to save layout work that is not the bottleneck.
   *
   * If the list ever routinely holds thousands, this is the one place that has to change,
   * and the row is already a fixed 76 px so it can.
   */

  const rows = $derived(queue.visible);
  const filtered = $derived(queue.jobs.length > 0 || queue.search.length > 0);

  let list: HTMLElement | null = $state(null);

  // Keyboard navigation moves `selected`; this is what makes the row it moved to visible.
  // `nearest` rather than `center` so a row already on screen does not make the list jump.
  $effect(() => {
    const id = queue.selected;
    if (id === null || !list) return;
    list.querySelector<HTMLElement>(`[data-job="${id}"]`)?.scrollIntoView({ block: "nearest" });
  });
</script>

<div class="list scroll" bind:this={list}>
  {#if rows.length === 0}
    <EmptyState {filtered} />
  {:else}
    <ul>
      {#each rows as job (job.id)}
        <JobRow {job} />
      {/each}
    </ul>
  {/if}
</div>

<style>
  .list {
    flex: 1;
    min-width: 0;
    background: var(--surface);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
</style>
