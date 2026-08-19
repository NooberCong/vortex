<script lang="ts">
  import type { Category } from "@vortex/proto";

  import { queue, type Filter } from "$lib/store.svelte";
  import Icon from "./Icon.svelte";

  /**
   * The only navigation there is (05 §Layout).
   *
   * No tabs, no toolbar, no ribbon — a download manager has one list, and everything else
   * is a way of looking at it. The two dividers are the structure: *state* above, *kind*
   * in the middle, *system* below. They carry meaning rather than decorating, which is why
   * there are exactly two of them and no `01 / 02 / 03` markers anywhere. A download list
   * is not a sequence.
   */

  interface Props {
    onSettings: () => void;
    settingsOpen: boolean;
  }

  const { onSettings, settingsOpen }: Props = $props();

  /** The four the spec names are always here; anything else appears once it has a job. */
  const ALWAYS: Category[] = ["Video", "Audio", "Archives", "Documents"];
  const ORDER: Category[] = [...ALWAYS, "Images", "Programs", "Other"];

  const categories = $derived(
    ORDER.filter((c) => ALWAYS.includes(c) || (queue.counts.byCategory.get(c) ?? 0) > 0),
  );

  const states = [
    { state: "active", label: "Active" },
    { state: "queued", label: "Queued" },
    { state: "done", label: "Done" },
  ] as const;

  function choose(filter: Filter): void {
    queue.filter = filter;
    if (settingsOpen) onSettings();
  }

  const active = $derived(queue.filter);
</script>

<nav class="sidebar" aria-label="Filters">
  <ul class="group">
    {#each states as item (item.state)}
      <li>
        <button
          class="item"
          aria-current={!settingsOpen && active.by === "state" && active.state === item.state}
          onclick={() => choose({ by: "state", state: item.state })}
        >
          <span class="label">{item.label}</span>
          <span class="count num">{queue.counts[item.state]}</span>
        </button>
      </li>
    {/each}
  </ul>

  <hr />

  <ul class="group">
    {#each categories as category (category)}
      <li>
        <button
          class="item"
          aria-current={!settingsOpen && active.by === "category" && active.category === category}
          onclick={() => choose({ by: "category", category })}
        >
          <span class="label">{category}</span>
          <span class="count num">{queue.counts.byCategory.get(category) ?? 0}</span>
        </button>
      </li>
    {/each}
  </ul>

  <hr />

  <ul class="group">
    <li>
      <button class="item" aria-current={settingsOpen} onclick={onSettings}>
        <Icon name="settings" />
        <span class="label">Settings</span>
      </button>
    </li>
  </ul>
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    width: 200px;
    flex: none;
    padding: var(--s3) var(--s3) var(--s4);
    border-right: 1px solid var(--rule);
    overflow-y: auto;
    scrollbar-width: none;
  }

  .group {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  hr {
    height: 1px;
    margin: var(--s1) var(--s2);
    border: none;
    background: var(--rule);
  }

  .item {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    height: 30px;
    padding: 0 var(--s3);
    border-radius: var(--radius-sm);
    color: var(--text-dim);
    transition:
      background var(--quick) var(--ease),
      color var(--quick) var(--ease);
  }

  .item:hover {
    background: color-mix(in oklab, var(--text) 5%, transparent);
    color: var(--text);
  }

  /*
   * Selection is weight and surface, never colour — there is no accent to reach for, and
   * that is the constraint doing its job (05 §The one rule).
   */
  .item[aria-current="true"] {
    background: var(--surface);
    color: var(--text);
    font-weight: 500;
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.06);
  }

  .label {
    flex: 1;
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .count {
    font-size: var(--t-meta);
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }

  .item[aria-current="true"] .count {
    color: var(--text-dim);
  }
</style>
