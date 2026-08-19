<script lang="ts">
  import type { Snippet } from "svelte";

  import Icon from "./Icon.svelte";

  /**
   * The one modal surface, centred in the window, 180 ms (05 §Screens).
   *
   * It used to drop from the top edge, on the argument that a panel arriving from the edge
   * of its own window reads as that window folding out rather than as a second application
   * demanding an answer. The argument is fine and the result was not: a panel pinned to the
   * top of a 760 px window sits above the optical centre and leaves a wedge of dead space
   * under it, and on a tall window it is nowhere near what the eye is looking at. Centred,
   * it lands where attention already is. It still grows out of nothing rather than sliding
   * in from somewhere, which is what keeps it from reading as an interruption.
   *
   * Modal, so it takes the keyboard: focus moves in on open, Tab cycles inside, Escape
   * leaves, and focus returns to whatever had it. Half of that is invisible to a mouse
   * user and all of it is the difference between "keyboard accessible" and "keyboard
   * usable".
   */

  interface Props {
    title: string;
    onClose: () => void;
    children: Snippet;
    /** Settings is a scrolling column and wants the room; a sheet with two fields does not. */
    wide?: boolean;
  }

  const { title, onClose, children, wide = false }: Props = $props();

  let panel: HTMLElement | null = $state(null);

  const FOCUSABLE =
    'a[href],button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),[tabindex]:not([tabindex="-1"])';

  $effect(() => {
    const restore = document.activeElement as HTMLElement | null;
    panel?.querySelector<HTMLElement>(FOCUSABLE)?.focus();
    return () => restore?.focus();
  });

  function keys(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.stopPropagation();
      onClose();
      return;
    }
    if (event.key !== "Tab" || !panel) return;

    const stops = [...panel.querySelectorAll<HTMLElement>(FOCUSABLE)];
    const first = stops[0];
    const last = stops[stops.length - 1];
    if (!first || !last) return;
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }
</script>

<svelte:window onkeydown={keys} />

<!--
  `pointerdown`, not `click`. Svelte delegates events at the root, so the very click that
  opened this sheet is still propagating when the scrim appears underneath the pointer — and
  a `click` handler here would catch it and close again before anything was drawn. The press
  that opened the sheet happened before the scrim existed, so it cannot reach it.
-->
<div class="scrim" role="presentation" onpointerdown={onClose}></div>

<div
  class="sheet"
  class:wide
  role="dialog"
  aria-modal="true"
  aria-label={title}
  bind:this={panel}
>
  <header>
    <h2>{title}</h2>
    <button class="button quiet square" aria-label="Close" onclick={onClose}>
      <Icon name="close" />
    </button>
  </header>
  <div class="content scroll">
    {@render children()}
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: color-mix(in oklab, var(--bg) 62%, transparent);
    backdrop-filter: blur(2px);
    animation: fade var(--calm) var(--ease);
    z-index: 10;
  }

  .sheet {
    position: fixed;
    top: 50%;
    left: 50%;
    z-index: 11;
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - var(--s7)));
    max-height: calc(100vh - var(--s7));
    background: var(--surface);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow);
    animation: rise var(--calm) var(--ease);
    transform: translate(-50%, -50%);
  }

  .sheet.wide {
    width: min(720px, calc(100vw - var(--s6)));
    height: calc(100vh - var(--s7));
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s3);
    padding: var(--s4) var(--s4) var(--s3) var(--s5);
    flex: none;
  }

  h2 {
    margin: 0;
    font-size: var(--t-name);
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .content {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    padding: 0 var(--s5) var(--s5);
    /* Without this the column refuses to shrink and the panel outgrows its max-height. */
    min-height: 0;
  }

  .square {
    width: 28px;
    height: 28px;
    padding: 0;
    color: var(--text-dim);
  }

  /* Grows in place. The 2% is deliberately almost nothing: enough that the panel arrives
     rather than appears, not enough to read as a zoom. */
  @keyframes rise {
    from {
      transform: translate(-50%, -50%) scale(0.98);
      opacity: 0;
    }
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }
</style>
