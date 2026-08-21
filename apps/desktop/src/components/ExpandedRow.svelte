<script lang="ts">
  import { moment, rate, ratio, took } from "@vortex/proto";
  import type { Protocol, WorkerFrame } from "@vortex/proto";
  import { untrack } from "svelte";

  import * as act from "$lib/actions";
  import { prompt } from "$lib/copy";
  import { CALM, exit, fold } from "$lib/motion";
  import { channel } from "$lib/segments";
  import { isDone, queue, type Job } from "$lib/store.svelte";
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

  /** How long the transfer took, once there are two instants to put between. */
  const elapsed = $derived(took(view.createdAt, view.finishedAt));
  /**
   * What each connection is doing, on the same 2 Hz clock as every other number in the
   * window — read off the 20 Hz frame rather than sent separately, since the daemon has no
   * per-worker summary and does not need one.
   *
   * The lane *list* stays live: a connection opening or closing is a change to what is on
   * screen, and the map beside it shows the same thing in the same instant. It is only the
   * figure that is held still long enough to read. A lane the sample has not caught up with
   * yet falls back to its own frame, so a new connection arrives with a speed rather than
   * with a dash.
   */
  let sampled = $state.raw(new Map<number, number>());
  $effect(() => {
    // The summary is the clock. Reading the frame under `untrack` is what keeps this at
    // 2 Hz instead of running it again on every one of the twenty frames a second that
    // arrive in between.
    void job.summary;
    sampled = new Map(untrack(() => job.detail?.workers ?? []).map((w) => [w.lane, w.bps]));
  });
  const ceiling = $derived(queue.settings?.maxConnections ?? null);
  const decision = $derived(view.state.kind === "needsDecision" ? prompt(view.state.decision) : null);
  /**
   * Whether there is a file at `destPath` to point at.
   *
   * Only a completed job has one. A failed job's path is where the file *would* have gone,
   * and offering "Show in folder" for it is a button whose only outcome is "that file isn't
   * there any more" — an error message the user asked for by following the interface's own
   * suggestion. A running job's bytes are in a `.vxpart` under a different name.
   */
  const saved = $derived(view.state.kind === "completed");
  /**
   * Whether this job is over for good, which is what decides if the map is drawn at all.
   *
   * The map is a picture of work in progress — which connections are live, which one is
   * stalling, the scheduler moving a lease off a slow worker. A terminal job has none of
   * that, and nothing can ever give it any back, so what is left to draw is a full bar:
   * a fatter copy of the one the collapsed body is already showing three lines above
   * (`JobRow.svelte`). Same reasoning as `Open` and `Show in folder` — an element whose
   * only possible outcome is nothing does not get to take up the room (05 §Expanded row).
   *
   * Not `saved`: a failed job is just as over. And not "has no live frame", which would
   * also catch paused — a paused job's map still answers "how much did it keep", and it
   * has a Resume button that turns it live again in the same place, with no jump.
   */
  const over = $derived(isDone(view.state));

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

<!--
  `data-fold` is how the lane maps inside this block learn that it is going: they run on the
  shared ticker and would otherwise keep drawing all the way through the collapse. See the
  shimmer effect in `SegmentMap.svelte`.
-->
<div
  class="detail"
  data-fold="here"
  in:fold={{ duration: CALM }}
  out:fold={{ duration: CALM, easing: exit }}
  onoutrostart={(event) => event.currentTarget.setAttribute("data-fold", "leaving")}
>
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

  {#if !over}
    <SegmentMap
      runs={detail?.runs ?? []}
      blocks={detail?.blocks ?? 0}
      height={28}
      perLane
      ratio={ratio(job.completed, view.total)}
      description="Per-connection segment map"
    />
  {/if}

  <div class="columns" class:facts-only={over}>
    {#if !over}
      <div class="workers">
        {#if workers.length === 0}
          <p class="meta idle">No connection is open right now.</p>
        {:else}
          {#each workers as worker (worker.lane)}
            <div class="worker" class:stealing={worker.stealingFrom}>
              <span class="lane num" style:color="var(--w{channel(worker.lane)})">
                w{worker.lane}
              </span>
              <span class="speed num">{rate(sampled.get(worker.lane) ?? worker.bps)}</span>
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
    {/if}

    <dl class="facts">
      {#if !over}
        <!-- Live, like the map: `0 of 16 · adaptive` on a job that ended half an hour ago
             is a true number about nothing, describing a controller that is not running. -->
        <dt class="micro">Connections</dt>
        <dd class="num">
          {frame?.connections ?? 0}{#if ceiling}<span class="dim">&nbsp;of {ceiling}</span>{/if}
          <span class="dim adaptive">adaptive</span>
        </dd>
      {/if}

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

      <!--
        The row's own stamp is one short word at the edge of the list, and it has to be — it
        is a column in forty rows. Here there is a label in front of every value, so this is
        where the abbreviating is undone: the whole instant, and the span between the two
        instants, which is the number this product is actually about. A row that says
        `4.9 GB` and `1m 12s` has made the argument for parallel connections without making
        a claim.
      -->
      <dt class="micro">Started</dt>
      <dd>{moment(view.createdAt)}</dd>

      {#if view.finishedAt}
        <dt class="micro">Finished</dt>
        <dd>
          {moment(view.finishedAt)}
          {#if elapsed}<span class="dim">· took <span class="num">{elapsed}</span></span>{/if}
        </dd>
      {/if}

      {#if view.media?.containerNote}
        <dt class="micro">Container</dt>
        <dd>{view.media.containerNote}</dd>
      {/if}
    </dl>
  </div>

  <div class="row-actions">
    <span class="path meta selectable" title={view.destPath}>{view.destPath}</span>
    {#if saved}
      <button class="button" onclick={() => act.open(view.destPath)}>Open</button>
      <button class="button" onclick={() => act.reveal(view.destPath)}>
        <Icon name="folder" />
        Show in folder
      </button>
    {/if}
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

  /*
   * Nothing to put beside the facts, so they take the reading column rather than sitting
   * out at the right-hand edge under an empty one. That alone is what unwraps
   * `Finished … · took 27m 6s`: the line needs about 330 px and the second column was 300.
   *
   * `max-content` rather than `1fr` because the two draw the same — nothing paints the
   * track, so a `1fr` list looks identical while claiming a box four times the width of
   * anything in it. `minmax(0, …)` keeps the 880 px cap for a long hostname, and `dd`'s
   * ellipsis takes it from there.
   */
  .columns.facts-only {
    grid-template-columns: minmax(0, max-content);
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
    /*
     * Full width, unlike the facts above it, and deliberately so. These are controls
     * rather than content: the path takes the slack and the buttons anchor to the right
     * edge of the panel, which puts them directly under the × and the open-external icon
     * in the row's own corner — both of those sit at `--s5` too, and `.detail` carries the
     * same `--s5` gutter. Capping this at the 880 px reading measure instead would leave
     * them floating at an edge nothing else in the window uses.
     */
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
