<script lang="ts" generics="T extends string | number">
  import Icon from "./Icon.svelte";

  /**
   * A dropdown, drawn in the page.
   *
   * A native `<select>` would be less code, and for a while it was. The problem is that
   * its list is not part of the page: Chromium renders that popup in the browser process,
   * so nothing in a stylesheet reaches it — not the surface colour, not the hairline, not
   * the hover, and not `cursor: pointer` on the options. In a frameless window with its
   * own palette the result is a slab of somebody else's operating system landing on top
   * of the interface, and a list whose items do not admit they are clickable.
   *
   * So the list is a listbox here instead. That buys the cursor and the palette, and costs
   * the keyboard — which is why all of it is implemented rather than half:
   *
   *   Space / Enter / ↓ / ↑   open, landing on the current value
   *   ↑ ↓ Home End            move the active option
   *   Enter                   commit          Escape  close, keeping the old value
   *   Tab / click elsewhere   close
   *
   * Focus never leaves the button; `aria-activedescendant` tells a screen reader which
   * option is current. That is the pattern with the fewest moving parts, because there is
   * only ever one focused element to put back.
   */

  interface Option {
    value: T;
    label: string;
  }

  interface Props {
    value: T;
    options: readonly Option[];
    onchange: (value: T) => void;
    /** Named by a `<label for=…>` in every current caller. */
    id?: string;
    /** For the cases with no visible label. */
    label?: string;
  }

  const { value, options, onchange, id, label }: Props = $props();

  let open = $state(false);
  let active = $state(0);
  let button: HTMLButtonElement | null = $state(null);
  let list: HTMLElement | null = $state(null);
  let box = $state({ top: 0, left: 0, width: 0, drop: true });

  const selected = $derived(options.find((option) => option.value === value));
  const listId = $derived(id ? `${id}-listbox` : undefined);

  /**
   * The list is `position: fixed`, because Settings is a scrolling column and an absolute
   * popup would be clipped by it. Fixed means it does not follow the button, so anything
   * that could move the button closes it instead — which is also what a native one does.
   */
  function place(): void {
    const rect = button?.getBoundingClientRect();
    if (!rect) return;
    const room = window.innerHeight - rect.bottom;
    const wanted = Math.min(options.length * 30 + 8, 260);
    box = {
      left: rect.left,
      width: rect.width,
      drop: room > wanted || room > rect.top,
      top: room > wanted || room > rect.top ? rect.bottom + 4 : rect.top - 4,
    };
  }

  function show(): void {
    active = Math.max(0, options.findIndex((option) => option.value === value));
    place();
    open = true;
  }

  function choose(index: number): void {
    const option = options[index];
    open = false;
    button?.focus();
    if (option && option.value !== value) onchange(option.value);
  }

  function keys(event: KeyboardEvent): void {
    if (!open) {
      if (event.key === "Enter" || event.key === " " || event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        show();
      }
      return;
    }

    switch (event.key) {
      case "Escape":
        // The sheet is also listening for Escape. One key, one thing (05 §Screens).
        event.stopPropagation();
        open = false;
        break;
      case "Enter":
      case " ":
        choose(active);
        break;
      case "ArrowDown":
        active = Math.min(options.length - 1, active + 1);
        break;
      case "ArrowUp":
        active = Math.max(0, active - 1);
        break;
      case "Home":
        active = 0;
        break;
      case "End":
        active = options.length - 1;
        break;
      case "Tab":
        open = false;
        return;
      default:
        return;
    }
    event.preventDefault();
  }

  /**
   * Moves the list to the end of `<body>`.
   *
   * Not tidiness — correctness. A `position: fixed` element resolves against the viewport
   * only if no ancestor is transformed, and the modal that contains every one of these
   * dropdowns is centred with `translate(-50%, -50%)`. Left in place, the list is
   * positioned relative to the panel and lands hundreds of pixels away from its button.
   */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  /** Keep the active option in view when the list is longer than its box. */
  $effect(() => {
    if (!open) return;
    list?.querySelector<HTMLElement>('[data-active="true"]')?.scrollIntoView({ block: "nearest" });
  });
</script>

<svelte:window
  onresize={() => (open = false)}
  onscroll={() => (open = false)}
  onpointerdown={(event) => {
    if (open && !button?.contains(event.target as Node) && !list?.contains(event.target as Node)) {
      open = false;
    }
  }}
/>

<button
  {id}
  bind:this={button}
  type="button"
  class="trigger"
  class:open
  role="combobox"
  aria-expanded={open}
  aria-controls={listId}
  aria-haspopup="listbox"
  aria-label={label}
  aria-activedescendant={open && listId ? `${listId}-${active}` : undefined}
  onclick={() => (open ? (open = false) : show())}
  onkeydown={keys}
>
  <span class="current">{selected?.label ?? ""}</span>
  <Icon name="chevron" />
</button>

{#if open}
  <!--
    `pointerdown` rather than `click` to commit: the window handler above closes the list
    on any press outside it, and a `click` on an option would arrive after that.
  -->
  <div
    use:portal
    bind:this={list}
    id={listId}
    class="list scroll"
    role="listbox"
    aria-label={label ?? "Options"}
    style="left:{box.left}px; width:{box.width}px; {box.drop ? 'top' : 'bottom'}:{box.drop
      ? box.top
      : window.innerHeight - box.top}px"
  >
    <!-- `tabindex="-1"` because the role demands focusability, not because anything here
         is ever focused: the button keeps focus and points at the active option with
         `aria-activedescendant`. -->
    {#each options as option, index (option.value)}
      <div
        id={listId ? `${listId}-${index}` : undefined}
        class="option"
        role="option"
        tabindex="-1"
        aria-selected={option.value === value}
        data-active={index === active}
        onpointerdown={(event) => {
          event.preventDefault();
          choose(index);
        }}
        onpointerenter={() => (active = index)}
      >
        {option.label}
      </div>
    {/each}
  </div>
{/if}

<style>
  .trigger {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s2);
    height: 30px;
    padding: 0 var(--s2);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    font-size: var(--t-body);
    color: var(--text);
    cursor: pointer;
    transition: border-color var(--quick) var(--ease);
  }

  .trigger:hover,
  .trigger.open {
    border-color: var(--text-faint);
  }

  .current {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .list {
    position: fixed;
    z-index: 20;
    max-height: 260px;
    padding: var(--s1);
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow);
    animation: lift var(--quick) var(--ease);
  }

  .option {
    padding: 0 var(--s2);
    height: 28px;
    display: flex;
    align-items: center;
    border-radius: var(--radius-sm);
    font-size: var(--t-body);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    /* The whole point of the exercise. */
    cursor: pointer;
  }

  .option[data-active="true"] {
    background: color-mix(in oklab, var(--text) 8%, transparent);
  }

  .option[aria-selected="true"] {
    font-weight: 500;
  }

  @keyframes lift {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
</style>
