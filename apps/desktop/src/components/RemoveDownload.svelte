<script lang="ts">
  import { bytes } from "@vortex/proto";
  import { tick } from "svelte";

  import * as act from "$lib/actions";
  import type { Job } from "$lib/store.svelte";
  import Sheet from "./Sheet.svelte";

  /**
   * Remove a finished download — and, if asked, the file it produced.
   *
   * The list and the disk are two different things, and the × on a row only ever meant the
   * first one. That is the right default and a bad silence: the file is the reason the row
   * existed, and a user who removes the row to tidy up has no way to know whether the
   * 4.9 GB went with it. So the app asks, once, at the only moment the answer is cheap —
   * the file is named, its size is on screen, and nothing has happened yet.
   *
   * Only for a *completed* job. Everything else has no finished file to lose: the daemon
   * writes into `.vxpart` and renames at the end, and `retire` deletes the partial work
   * whatever the answer is. A dialog whose two branches do the same thing is a dialog that
   * teaches people to stop reading dialogs — so `requestRemove` never opens one.
   *
   * The safe branch is the default and the one Enter takes (05 §Copy: never a destructive
   * one). Arming the other is a deliberate act, it says what it will cost in bytes, and the
   * button that carries it out renames itself to say what it is about to do — a button
   * whose verb does not match its consequence is how an interface loses trust.
   */

  interface Props {
    job: Job;
    onClose: () => void;
  }

  const { job, onClose }: Props = $props();

  let alsoDelete = $state(false);
  let affirm: HTMLButtonElement | null = $state(null);

  /**
   * Enter answers the question the user came here with, not the one the sheet's × asks.
   *
   * `Sheet` focuses its first control on open, which is the close button — right for a
   * screen, wrong for a question. After a `tick` that flush is done and this can take the
   * keyboard back. Safe to make the default because the destructive half is a separate,
   * deliberate act: pressing Enter straight away removes the row and keeps the file.
   */
  $effect(() => {
    void tick().then(() => affirm?.focus());
  });

  const view = $derived(job.view);
  /** The finished size — `total` is set to the byte count on completion, so it is exact. */
  const size = $derived(view.total == null ? null : bytes(view.total));

  function confirm(): void {
    void act.remove(job.id, alsoDelete);
    onClose();
  }
</script>

<Sheet title="Remove download" {onClose}>
  <div class="file">
    <p class="filename">{view.filename}</p>
    <p class="where meta num" title={view.destPath}>{view.destPath}</p>
  </div>

  <!--
    A label, so the whole strip is the hit target — a 3 mm checkbox is a target you have to
    aim at, and this is the one control in the sheet that changes what the button does.
  -->
  <label class="option" class:armed={alsoDelete}>
    <input type="checkbox" bind:checked={alsoDelete} />
    <span class="box" aria-hidden="true">
      <svg viewBox="0 0 12 12" width="12" height="12">
        <path
          d="M2.5 6.2 4.8 8.5 9.5 3.8"
          fill="none"
          stroke="currentColor"
          stroke-width="1.75"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </span>
    <span class="text">
      <span class="lede">Also delete the file from disk</span>
      <!--
        The two sentences are deliberately the same length. They swap in place, directly
        above the button the user is on their way to, and a note that grows a line when
        ticked would move that button out from under the pointer.
      -->
      <span class="note meta">
        {#if alsoDelete}
          {size ? `Frees ${size}.` : "The file goes."} There is no recycle bin, so this
          cannot be undone.
        {:else}
          The file stays where it is{size ? `, all ${size} of it` : ""}. Only the row goes.
        {/if}
      </span>
    </span>
  </label>

  <div class="actions">
    <button class="button" onclick={onClose}>Cancel</button>
    <button class="button primary" bind:this={affirm} onclick={confirm}>
      {alsoDelete ? "Remove and delete" : "Remove"}
    </button>
  </div>
</Sheet>

<style>
  .file {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
  }

  .filename {
    margin: 0;
    font-size: var(--t-name);
    font-weight: 500;
    word-break: break-all;
  }

  .where {
    margin: 0;
    color: var(--text-faint);
    /* One line: the path is orientation, not the subject. The full thing is in the title. */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /*
   * The armed state is the one place colour is allowed here, and it is spent the way the
   * tokens say attention is spent: a 2 px left edge, never a fill. A red button would be
   * the same information in the loudest possible register, and would make every other
   * destructive moment in the app look safe by comparison.
   */
  .option {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: var(--s3);
    align-items: start;
    padding: var(--s3);
    border: 1px solid var(--rule);
    border-left: 2px solid var(--rule);
    border-radius: var(--radius);
    background: var(--bg);
    transition: border-color var(--quick) var(--ease);
  }

  .option:hover {
    border-color: var(--rule-strong);
    border-left-color: var(--rule-strong);
  }

  .option.armed,
  .option.armed:hover {
    border-left-color: var(--attention);
  }

  /* Invisible rather than absent — the difference between a control and a picture of one. */
  input {
    position: absolute;
    opacity: 0;
    width: 0;
    height: 0;
  }

  .box {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    /* Optical alignment with the cap height of the line beside it, not its box. */
    margin-top: 1px;
    border: 1px solid var(--rule-strong);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: transparent;
    transition:
      background var(--quick) var(--ease),
      border-color var(--quick) var(--ease),
      color var(--quick) var(--ease);
  }

  input:checked + .box {
    background: var(--text);
    border-color: var(--text);
    color: var(--surface);
  }

  input:focus-visible + .box {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    min-width: 0;
  }

  .lede {
    font-size: var(--t-body);
  }

  .note {
    color: var(--text-faint);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--s2);
    padding-top: var(--s2);
    border-top: 1px solid var(--rule);
  }
</style>
