<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { rate } from "@vortex/proto";

  import { aggregate } from "$lib/aggregate.svelte";
  import { native } from "$lib/ipc";
  import { CALM, fade } from "$lib/motion";
  import { queue } from "$lib/store.svelte";
  import Icon from "./Icon.svelte";
  import Mark from "./Mark.svelte";
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
    onSettings: () => void;
    searching: boolean;
    settingsOpen: boolean;
  }

  const { onAdd, onSearch, onSettings, searching, settingsOpen }: Props = $props();

  const mac = navigator.userAgent.includes("Mac");
  const speed = $derived(rate(queue.throughput));

  let field: HTMLInputElement | null = $state(null);

  /**
   * The field is always in the DOM now, so `autofocus` — which only fires on insertion —
   * no longer applies. Ctrl-F has to hand it the keyboard itself.
   */
  $effect(() => {
    if (searching) field?.focus();
  });

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
  <div class="brand" data-tauri-drag-region>
    <Mark spinning={queue.running > 0} />
    <span class="wordmark" data-tauri-drag-region>vortex</span>
  </div>

  <div class="readout" data-tauri-drag-region>
    {#if queue.running > 0}
      <!--
        The readout arrives and leaves whenever the queue starts and stops, which on a busy
        machine is often. It fades rather than blinks; its slot is reserved either way, so
        nothing beside it moves.
      -->
      <div class="figures" transition:fade={{ duration: CALM }}>
        <span class="speed num">{speed}</span>
        <Sparkline values={aggregate.history} width={72} height={18} faint />
      </div>
    {/if}
  </div>

  <div class="controls">
    <!--
      The button and the field are one control that changes width, not two controls that
      swap. Swapping meant 150 px of bar appearing between two frames and everything to the
      right of it jumping left — for the shortcut people reach for most often after Add.
      Both are always here; which one is lit is opacity, and the box they share is what
      moves. `inert` keeps the dark one out of the tab order and out of the accessibility
      tree, so there is still only ever one thing here to find.
    -->
    <div class="find" class:open={searching}>
      <button
        class="button quiet square"
        title="Filter (Ctrl F)"
        aria-label="Filter downloads"
        inert={searching}
        onclick={onSearch}
      >
        <Icon name="search" />
      </button>
      <input
        class="search num"
        type="search"
        placeholder="Filter"
        aria-label="Filter downloads"
        inert={!searching}
        bind:this={field}
        bind:value={queue.search}
        onblur={() => queue.search === "" && onSearch()}
        onkeydown={(e) => e.key === "Escape" && (queue.search = "", onSearch())}
      />
    </div>

    <!--
      Settings was the last entry in the left column, under the second divider, and it was
      the only thing in there that was not a way of looking at the list. It is a sheet, so
      it was never really a destination either — `aria-current` says it is open, not that
      you are somewhere.
    -->
    <button
      class="button quiet square"
      title="Settings (Ctrl ,)"
      aria-label="Settings"
      aria-current={settingsOpen}
      onclick={onSettings}
    >
      <Icon name="settings" />
    </button>

    <!--
      An icon, like the two beside it. It was a bordered button reading "+ Add", which is
      the one control in the bar that says what it does — and that is exactly what made it
      heavy: a filled 68 px block against a 40 px bar whose whole job is to stay out of the
      way of the list. Three squares of the same size read as one set of tools rather than
      as a button with two ornaments next to it.

      It keeps the last position, so the bar still ends on the thing you came to press, and
      it keeps its name in the tooltip and the accessible label. It is the only glyph in
      the set that is a plus, and a plus in the corner of a window that holds a list has
      exactly one meaning.
    -->
    <button
      class="button quiet square"
      title="New download (Ctrl N)"
      aria-label="New download"
      onclick={onAdd}
    >
      <Icon name="plus" />
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

  .brand {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex: none;
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
    min-width: 0;
    /* Reserved: the readout appears and disappears as the queue starts and stops, and the
       controls must not move when it does. */
    height: 100%;
  }

  .figures {
    display: flex;
    align-items: center;
    gap: var(--s3);
    min-width: 0;
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

  /* Open, not "here". The sheet is over the window; this only says which one it is. */
  .square[aria-current="true"] {
    background: color-mix(in oklab, var(--text) 8%, transparent);
    color: var(--text);
  }

  /* The box the two of them share. Closed it is exactly a square button; open it is a
     field, and the controls to its right slide over rather than jump. */
  .find {
    position: relative;
    flex: none;
    width: 30px;
    height: 30px;
    transition: width var(--calm) var(--ease);
  }

  .find.open {
    width: 180px;
  }

  .find > .square {
    position: absolute;
    inset: 0 auto 0 0;
    transition: opacity var(--quick) var(--ease);
  }

  .find.open > .square {
    opacity: 0;
  }

  .search {
    position: absolute;
    inset: 0;
    cursor: text;
    width: 100%;
    padding: 0 var(--s3);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    font-size: var(--t-body);
    font-family: var(--font-ui);
    opacity: 0;
    transition: opacity var(--quick) var(--ease);
  }

  .find.open .search {
    opacity: 1;
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
