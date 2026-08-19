<script lang="ts">
  import { rate, ratio } from "@vortex/proto";
  import type { Protocol, WorkerFrame } from "@vortex/proto";

  import * as act from "$lib/actions";
  import { prompt } from "$lib/copy";
  import { channel } from "$lib/segments";
  import { queue, type Job } from "$lib/store.svelte";
  import Icon from "./Icon.svelte";
  import SegmentMap from "./SegmentMap.svelte";
  import Sparkline from "./Sparkline.svelte";

  /**
   * The row, opened.
   *
   * This is where the segment map earns the space it takes: one band per connection, so
   * the scheduler's behaviour is legible rather than merely claimed. A worker that stalls
   * is a band that stops. A worker that finishes and takes the back half of the slowest
   * lease is two bands changing shape at once, and you watch it happen.
   *
   * `Retries — 2 · both recovered` is here on purpose. Showing that the engine hit trouble
   * and dealt with it builds more trust than hiding it, and it is the resilience story
   * made visible (05 §Expanded row).
   */

  interface Props {
    job: Job;
  }

  const { job }: Props = $props();

  const view = $derived(job.view);
  const detail = $derived(job.detail);
  const frame = $derived(job.frame);
  const workers = $derived<WorkerFrame[]>(detail?.workers ?? []);
  const ceiling = $derived(queue.settings?.maxConnections ?? null);
  const decision = $derived(view.state.kind === "needsDecision" ? prompt(view.state.decision) : null);

  function protocol(p: Protocol | null | undefined): string {
    if (p === "H3") return "HTTP/3 · QUIC";
    if (p === "H2") return "HTTP/2";
    if (p === "Http11") return "HTTP/1.1";
    return "—";
  }

  /** `2 · both recovered`, and the two neighbouring cases that would otherwise read wrong. */
  function retries(total: number, recovered: number): string {
    if (total === 0) return "None";
    if (recovered < total) return `${total} · ${recovered} recovered`;
    if (total === 1) return "1 · recovered";
    if (total === 2) return "2 · both recovered";
    return `${total} · all recovered`;
  }
</script>

<div class="detail">
  {#if decision}
    <!-- A question only the user can answer, in the interface's voice, with the fix
         attached. Never a code, never an apology (05 §Copy). -->
    <div class="prompt">
      <div class="prompt-text">
        <strong>{decision.title}</strong>
        {#if decision.detail}<span class="meta">{decision.detail}</span>{/if}
      </div>
      <div class="choices">
        {#each decision.choices as choice (choice.label)}
          <button
            class="button"
            class:primary={choice.preferred}
            onclick={() => choice.resolution && act.decide(job.id, choice.resolution)}
          >
            {choice.label}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  <SegmentMap
    runs={detail?.runs ?? []}
    blocks={detail?.blocks ?? 0}
    height={28}
    perLane
    ratio={ratio(job.completed, view.total)}
    description="Per-connection segment map"
  />

  <div class="columns">
    <div class="workers">
      {#if workers.length === 0}
        <p class="meta idle">No connection is open right now.</p>
      {:else}
        {#each workers as worker (worker.lane)}
          <div class="worker" class:stealing={worker.stealingFrom}>
            <span class="lane num" style:color="var(--w{channel(worker.lane)})">
              w{worker.lane}
            </span>
            <span class="speed num">{rate(worker.bps)}</span>
            <Sparkline values={worker.spark} channel={channel(worker.lane)} />
            {#if worker.stealingFrom}
              <!-- Named as well as drawn: the map shows the lease shrinking, and a user
                   who has not learned to read the map yet gets the sentence. -->
              <span class="micro steal">being stolen from</span>
            {/if}
          </div>
        {/each}
      {/if}
    </div>

    <dl class="facts">
      <dt class="micro">Connections</dt>
      <dd class="num">
        {frame?.connections ?? 0}{#if ceiling}<span class="dim">&nbsp;of {ceiling}</span>{/if}
        <span class="dim adaptive">adaptive</span>
      </dd>

      <dt class="micro">Protocol</dt>
      <dd>{protocol(view.protocol)}</dd>

      <dt class="micro">Source</dt>
      <dd>
        <span class="num">{view.addresses}</span>
        {view.addresses === 1 ? "address" : "addresses"} · {view.host}
      </dd>

      {#if view.verified}
        <dt class="micro">Verified</dt>
        <dd>{view.verified.algorithm} {view.verified.ok ? "✓" : "✗"}</dd>
      {/if}

      <dt class="micro">Retries</dt>
      <dd>{retries(view.retries.total, view.retries.recovered)}</dd>

      {#if view.media?.containerNote}
        <dt class="micro">Container</dt>
        <dd>{view.media.containerNote}</dd>
      {/if}
    </dl>
  </div>

  <div class="row-actions">
    <span class="path meta selectable" title={view.destPath}>{view.destPath}</span>
    {#if view.state.kind === "completed"}
      <button class="button" onclick={() => act.open(view.destPath)}>Open</button>
    {/if}
    <button class="button" onclick={() => act.reveal(view.destPath)}>
      <Icon name="folder" />
      Show in folder
    </button>
  </div>
</div>

<style>
  .detail {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    /* Its own gutter: the collapsed body above carries the row's padding, and this is a
       sibling of it rather than a child. Same 24 px, so the map lines up with the bar. */
    padding: var(--s2) var(--s5) var(--s5);
    /* The expanded body is one 180 ms reveal, matching every other standard move. */
    animation: open var(--calm) var(--ease);
  }

  @keyframes open {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }

  .prompt {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    padding: var(--s3) var(--s4);
    border: 1px solid var(--rule-strong);
    border-left: 2px solid var(--attention);
    border-radius: var(--radius-sm);
  }

  .prompt-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .prompt-text strong {
    font-weight: 500;
  }

  .choices {
    display: flex;
    gap: var(--s2);
    flex: none;
  }

  /* Content, not chrome: it stops at a readable width instead of stretching a five-line
     list across a maximised window. The map above is the exception — it is a map of the
     file, so it uses every pixel the file has. */
  .columns {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    gap: var(--s6);
    align-items: start;
    max-width: 880px;
  }

  .workers {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .worker {
    display: flex;
    align-items: center;
    gap: var(--s3);
    height: 20px;
    font-size: var(--t-meta);
  }

  .lane {
    width: 26px;
    font-size: var(--t-micro);
    font-weight: 500;
  }

  .speed {
    width: 82px;
    text-align: right;
    color: var(--text-dim);
  }

  .steal {
    color: var(--text-faint);
  }

  .idle {
    margin: 0;
  }

  .facts {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    gap: var(--s2) var(--s3);
    margin: 0;
    font-size: var(--t-meta);
  }

  .facts dt {
    color: var(--text-faint);
    line-height: 16px;
  }

  .facts dd {
    margin: 0;
    color: var(--text);
    line-height: 16px;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .dim {
    color: var(--text-dim);
  }

  /* The adaptive controller is the feature; a user who set 32 and sees 12 in use needs to
     know that is correct (05 §Settings). */
  .adaptive {
    margin-left: var(--s2);
    font-size: var(--t-micro);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-faint);
  }

  .row-actions {
    display: flex;
    align-items: center;
    gap: var(--s2);
    max-width: 880px;
  }

  .path {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-faint);
  }
</style>
