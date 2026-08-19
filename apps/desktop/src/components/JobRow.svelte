<script lang="ts">
  import { bytes, eta, percent, rate, ratio } from "@vortex/proto";

  import * as act from "$lib/actions";
  import { label, note, primaryAction } from "$lib/copy";
  import { expand, needsAttention, queue, type Job } from "$lib/store.svelte";
  import ExpandedRow from "./ExpandedRow.svelte";
  import Icon from "./Icon.svelte";
  import SegmentMap from "./SegmentMap.svelte";

  /**
   * One job.
   *
   * The row says four things and stops: what it is, where the bytes are, how it is going,
   * and — only when there is one — the word for a state that is not "downloading". A job
   * that is moving carries no state label at all, because the map is already moving and is
   * already the only coloured thing on screen. Saying it twice is how a list stops being
   * scannable.
   *
   * Every numeric field sits in a fixed-width box, and the state label and the hover
   * actions share one fixed slot. The row does not reflow when the speed goes from 9 to
   * 12 MB/s, when the ETA drops a digit, or when the pointer arrives
   * (05 §Numbers that don't lie or twitch).
   */

  interface Props {
    job: Job;
  }

  const { job }: Props = $props();

  const view = $derived(job.view);
  const state = $derived(view.state);
  const frame = $derived(job.frame);
  const expanded = $derived(queue.expanded === job.id);
  const selected = $derived(queue.selected === job.id);
  const action = $derived(primaryAction(state));
  const sentence = $derived(note(state));

  const done = $derived(state.kind === "completed");
  const complete = $derived(percent(job.completed, view.total));

  /**
   * The line under the filename.
   *
   * Numeric fields are marked as such rather than assumed: every numeral in the app is
   * tabular mono, and every word is not. A sentence set in a monospace face is the tell of
   * a developer tool, and this is not one.
   */
  type Field = { text: string; num: boolean };
  const word = (text: string): Field => ({ text, num: false });
  const figure = (text: string): Field => ({ text, num: true });

  const meta = $derived.by((): Field[] => {
    if (sentence) return [word(sentence)];
    if (state.kind === "muxing") {
      const percent = view.media?.muxPercent;
      const parts = [word("Combining video and audio")];
      if (percent != null) parts.push(figure(`${percent}%`));
      return parts;
    }
    if (done) {
      const parts = [figure(bytes(view.completed)), word(view.host)];
      if (view.verified?.ok) parts.push(word(`${view.verified.algorithm} ✓`));
      return parts;
    }
    const parts = [
      figure(complete),
      figure(`${bytes(job.completed)} of ${view.total ? bytes(view.total) : "?"}`),
    ];
    if (state.kind === "downloading") {
      parts.push(figure(rate(job.bps)));
      const remaining = eta(frame?.etaSecs);
      if (remaining) parts.push(figure(remaining));
      if (frame?.connections) parts.push(figure(String(frame.connections)));
    }
    return parts;
  });

  /** Colour is never the sole carrier of meaning, so the map has words too. */
  const description = $derived(
    done
      ? "Complete"
      : `${complete} complete${frame?.connections ? `, ${frame.connections} connections` : ""}`,
  );

  function toggle(): void {
    queue.selected = job.id;
    void expand(job.id);
  }

  function keys(event: KeyboardEvent): void {
    if (event.key !== "Enter") return;
    event.preventDefault();
    toggle();
  }
</script>

<li class="row" data-job={job.id} class:selected class:expanded class:attention={needsAttention(state)}>
  <!--
    The body is the click and keyboard target; the actions are its sibling rather than its
    children, because a button inside a button is not a thing a screen reader can describe.
    They overlap on screen — one fixed 88 px slot, occupied by the state label until the
    pointer arrives — so the filename's width never changes.
  -->
  <div
    class="body"
    role="button"
    tabindex="0"
    aria-expanded={expanded}
    aria-label="{view.filename}. {label(state) ?? 'Downloading'}. {description}"
    onclick={toggle}
    onkeydown={keys}
    onfocus={() => (queue.selected = job.id)}
  >
    <div class="line">
      <span class="name" title={view.destPath}>{view.filename}</span>
      <span class="slot micro state">{label(state) ?? ""}</span>
    </div>

    <SegmentMap
      runs={frame?.runs ?? []}
      blocks={frame?.blocks ?? 0}
      ratio={ratio(job.completed, view.total)}
      {description}
    />

    <div class="line meta">
      {#each meta as part, i (i)}
        {#if i > 0}<span class="dot" aria-hidden="true">·</span>{/if}
        <span class="field" class:num={part.num}>{part.text}</span>
      {/each}
    </div>
  </div>

  <div class="actions">
    {#if done}
      <button class="button quiet icon" title="Show in folder" onclick={() => act.reveal(view.destPath)}>
        <Icon name="reveal" />
      </button>
    {:else if action}
      <button
        class="button quiet icon"
        title={action === "pause" ? "Pause" : "Resume"}
        onclick={() => (action === "pause" ? act.pause(job.id) : act.resume(job.id))}
      >
        <Icon name={action === "pause" ? "pause" : "play"} />
      </button>
    {/if}
    <button class="button quiet icon" title="Remove" onclick={() => act.remove(job.id)}>
      <Icon name="close" />
    </button>
  </div>

  {#if expanded}
    <ExpandedRow {job} />
  {/if}
</li>

<style>
  .row {
    position: relative;
    border-bottom: 1px solid var(--rule);
    transition: background var(--quick) var(--ease);
  }

  /*
   * The 76 px row, composed exactly: 6 top, a 26 px first line, 8, the 6 px map, 8, a
   * 16 px meta line, 6 bottom. Fixed rather than centred so the actions can sit at a known
   * offset, and so the geometry does not change when the row expands.
   */
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    height: 76px;
    padding: 6px var(--s5);
    cursor: pointer;
  }

  .row:hover {
    background: color-mix(in oklab, var(--text) 3%, transparent);
  }

  .row.selected {
    background: color-mix(in oklab, var(--text) 5%, transparent);
  }

  /*
   * Attention is a 2 px left edge. Never a fill and never a background (05 §Tokens) — a
   * red row is an alarm, and a job waiting for an answer is not an alarm.
   */
  .row.attention::before {
    content: "";
    position: absolute;
    inset: 0 auto 0 0;
    width: 2px;
    background: var(--attention);
  }

  .line {
    display: flex;
    align-items: center;
    gap: var(--s2);
    min-width: 0;
  }

  .line:first-child {
    height: 26px;
  }

  .name {
    flex: 1;
    min-width: 0;
    font-size: var(--t-name);
    font-weight: 500;
    letter-spacing: -0.005em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .slot {
    flex: none;
    width: 88px;
    text-align: right;
    color: var(--text-faint);
    transition: opacity var(--quick) var(--ease);
  }

  .actions {
    position: absolute;
    top: 6px;
    right: var(--s5);
    display: flex;
    align-items: center;
    gap: var(--s1);
    height: 26px;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--quick) var(--ease);
  }

  .row:hover .actions,
  .row:focus-within .actions {
    opacity: 1;
    pointer-events: auto;
  }

  .row:hover .slot,
  .row:focus-within .slot {
    opacity: 0;
  }

  .meta {
    font-size: var(--t-meta);
    color: var(--text-dim);
    height: 16px;
    overflow: hidden;
  }

  .dot {
    color: var(--text-faint);
    flex: none;
  }

  /*
   * Fixed fields — percentage, then the size pair, then speed, ETA and the connection
   * count, each in a box wide enough for its largest plausible value. The row is still
   * when only the digits change.
   */
  .field {
    flex: none;
    white-space: nowrap;
  }

  /* Words shrink when the row is narrow but never grow: a sentence that expanded would
     push the fields after it to the far edge of the row. */
  .field:not(.num) {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta .field:nth-child(1) {
    min-width: 30px;
  }

  .meta .field:nth-child(3) {
    min-width: 130px;
  }

  .meta .field:nth-child(5) {
    min-width: 74px;
  }

  .meta .field:nth-child(7) {
    min-width: 50px;
  }

  .icon {
    width: 26px;
    height: 26px;
    padding: 0;
    color: var(--text-dim);
    background: var(--surface);
  }

  .icon:hover {
    color: var(--text);
  }
</style>
