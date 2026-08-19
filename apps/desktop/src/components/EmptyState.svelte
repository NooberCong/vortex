<script lang="ts">
  import { queue } from "$lib/store.svelte";

  /**
   * An invitation to act, in the interface's voice. Not an illustration, not a mascot
   * (05 §Empty state).
   *
   * The important case is the one that is not empty at all: an empty list because there
   * are no downloads and an empty list because the daemon is not running look identical,
   * and letting a user conclude their queue was lost is a far worse failure than any
   * wording. So the disconnected state says so first, and says what is being done about
   * it — the app is already trying to start `vortexd`, and it will connect by itself.
   */

  interface Props {
    filtered: boolean;
  }

  const { filtered }: Props = $props();
</script>

<div class="empty">
  {#if !queue.connected}
    <p class="display">Starting Vortex…</p>
    <p class="line">
      Downloads keep running even when this window is closed,<br />
      so nothing has been lost.
    </p>
  {:else if filtered}
    <p class="display">Nothing here.</p>
    <p class="line">Try another section, or clear the filter.</p>
  {:else}
    <p class="display">Nothing downloading.</p>
    <p class="line">
      Drop a link here, or press <kbd>Ctrl</kbd> <kbd>N</kbd>.<br />
      Vortex catches downloads from your browser automatically.
    </p>
  {/if}
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--s3);
    height: 100%;
    padding: var(--s7);
    text-align: center;
  }

  .display {
    margin: 0;
    font-size: var(--t-display);
    font-weight: 600;
    letter-spacing: -0.02em;
    color: var(--text);
  }

  .line {
    margin: 0;
    max-width: 42ch;
    color: var(--text-dim);
    line-height: 1.6;
  }

  kbd {
    display: inline-block;
    min-width: 20px;
    padding: 1px 5px;
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    font-family: var(--font-num);
    font-size: var(--t-meta);
    color: var(--text);
  }
</style>
