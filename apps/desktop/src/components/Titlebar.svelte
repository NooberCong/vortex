<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { rate } from "@vortex/proto";

  import { aggregate } from "$lib/aggregate.svelte";
  import { native } from "$lib/ipc";
  import { queue } from "$lib/store.svelte";
  import Icon from "./Icon.svelte";
  import Sparkline from "./Sparkline.svelte";

  /**
   * The titlebar (05 §Layout).
   *
   * Frameless, 40 px, and draggable everywhere that is not a control. The aggregate
   * throughput lives here rather than in a row because it is the one number that is true
   * of the whole app — and it is the app's only display-sized type, which is what makes
   * looking at the window for half a second worth something.
   *
   * On macOS the traffic lights are the system's, inset 20/20, so the left edge makes room
   * for them and no buttons are drawn. Everywhere else the three controls are ours.
   */

  interface Props {
    onAdd: () => void;
    onSearch: () => void;
    searching: boolean;
  }

  const { onAdd, onSearch, searching }: Props = $props();

  const mac = navigator.userAgent.includes("Mac");
  const speed = $derived(rate(queue.throughput));

  async function control(action: "minimise" | "maximise" | "close"): Promise<void> {
    if (!native) return;
    const window = getCurrentWindow();
    if (action === "minimise") await window.minimize();
    else if (action === "maximise") await window.toggleMaximize();
    // Close hides to the tray; the daemon keeps downloading either way. See `lib.rs`.
    else await window.close();
  }
</script>

<header class="titlebar" data-tauri-drag-region class:mac>
  <span class="wordmark" data-tauri-drag-region>vortex</span>

  <div class="readout" data-tauri-drag-region>
    {#if queue.running > 0}
      <span class="speed num">{speed}</span>
      <Sparkline values={aggregate.history} width={72} height={18} faint />
    {/if}
  </div>

  <div class="controls">
    {#if searching}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="search num"
        type="search"
        autofocus
        placeholder="Filter"
        aria-label="Filter downloads"
        bind:value={queue.search}
        onblur={() => queue.search === "" && onSearch()}
        onkeydown={(e) => e.key === "Escape" && (queue.search = "", onSearch())}
      />
    {:else}
      <button class="button quiet square" title="Filter (Ctrl F)" onclick={onSearch}>
        <Icon name="search" />
      </button>
    {/if}

    <button class="button" onclick={onAdd}>
      <Icon name="plus" />
      Add
    </button>
  </div>

  {#if !mac}
    <div class="window-controls">
      <button class="chrome" aria-label="Minimise" onclick={() => control("minimise")}>
        <Icon name="minimise" size={12} />
      </button>
      <button class="chrome" aria-label="Maximise" onclick={() => control("maximise")}>
        <Icon name="maximise" size={10} />
      </button>
      <button class="chrome close" aria-label="Close" onclick={() => control("close")}>
        <Icon name="close" size={12} />
      </button>
    </div>
  {/if}
</header>

<style>
  .titlebar {
    display: flex;
    align-items: center;
    gap: var(--s4);
    height: 40px;
    flex: none;
    padding-left: var(--s5);
    border-bottom: 1px solid var(--rule);
    /* Nothing in the titlebar may be dragged as text; the whole bar is a window handle. */
    user-select: none;
  }

  /* Room for the traffic lights, which the system draws at 20/20. */
  .titlebar.mac {
    padding-left: 84px;
  }

  .wordmark {
    font-size: var(--t-body);
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--text);
    flex: none;
  }

  .readout {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: flex-start;
    gap: var(--s3);
    min-width: 0;
    /* Reserved: the readout appears and disappears as the queue starts and stops, and the
       Add button must not move when it does. */
    height: 100%;
  }

  .speed {
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -0.02em;
    line-height: 1;
    color: var(--text);
  }

  .controls {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex: none;
    padding-right: var(--s3);
  }

  .square {
    width: 30px;
    padding: 0;
    color: var(--text-dim);
  }

  .search {
    cursor: text;
    width: 180px;
    height: 30px;
    padding: 0 var(--s3);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    font-size: var(--t-body);
    font-family: var(--font-ui);
  }

  .search::placeholder {
    color: var(--text-faint);
  }

  .window-controls {
    display: flex;
    align-self: stretch;
    flex: none;
  }

  .chrome {
    display: grid;
    place-items: center;
    width: 46px;
    align-self: stretch;
    color: var(--text-dim);
    transition:
      background var(--quick) var(--ease),
      color var(--quick) var(--ease);
  }

  .chrome:hover {
    background: color-mix(in oklab, var(--text) 8%, transparent);
    color: var(--text);
  }

  /*
   * The one exception to "colour is reserved for data", and it is not ours to make: every
   * user already knows the close button turns red, and a grey one reads as broken. It
   * borrows the app's single red rather than introducing a second one.
   */
  .chrome.close:hover {
    background: var(--attention);
    color: #fff;
  }
</style>
