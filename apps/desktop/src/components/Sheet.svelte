<script lang="ts">
  import type { Snippet } from "svelte";

  import { CALM, exit, fade, grow } from "$lib/motion";
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
   *
   * It leaves the way it came, which it used to not. The enter was a CSS animation, and CSS
   * cannot animate a node that is about to stop existing — so every Escape, every ×, every
   * click on the scrim ended with the panel and the blur gone between two frames. That is
   * the most-repeated moment in the app and it was the one with no motion in it at all.
   * These are Svelte transitions instead, so both directions exist, and the way out runs on
   * the faster curve (`motion.ts`): the answer has been given and the eye is already back
   * on the list.
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

  /**
   * On its way out, and therefore no longer a target.
   *
   * A fading scrim is still a full-window hit area and a fading panel still has a live
   * Cancel button in it. 180 ms is short, and it is long enough to eat the first click of
   * whatever the user turned to next — so the moment either starts leaving, both stop
   * catching anything.
   */
  let leaving = $state(false);

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
<div
  class="scrim"
  class:leaving
  role="presentation"
  onpointerdown={onClose}
  in:fade={{ duration: CALM }}
  out:fade={{ duration: CALM, easing: exit }}
  onoutrostart={() => (leaving = true)}
></div>

<div
  class="sheet"
  class:wide
  class:leaving
  role="dialog"
  aria-modal="true"
  aria-label={title}
  bind:this={panel}
  in:grow={{ duration: CALM }}
  out:grow={{ duration: CALM, easing: exit }}
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
    z-index: 10;
  }

  .leaving {
    pointer-events: none;
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
</style>
