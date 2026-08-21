import type {
  Category,
  Event,
  JobId,
  JobState,
  JobView,
  ProgressFrame,
  ProbeResult,
  Settings,
  SummaryFrame,
} from "@vortex/proto";

import { tick } from "svelte";

import { daemon } from "./ipc";

/**
 * The window's model of the queue.
 *
 * It is a projection, not a source. Every field here was put there by an `Event`, and the
 * only way it changes is another `Event` — a command is sent and the UI waits to be told
 * what happened rather than guessing. That costs one round trip through a named pipe on
 * the same machine, and it buys the property that two open windows, the CLI and the tray
 * can never disagree about whether a job is paused.
 *
 * Frames are `$state.raw`. A `ProgressFrame` arrives twenty times a second carrying an
 * array of forty tuples; making that array deeply reactive would allocate a proxy per run
 * per frame to detect a mutation that never happens, because frames are replaced whole.
 */

/** One row. Its three fields arrive on three different cadences and are never merged. */
export class Job {
  view = $state.raw<JobView>() as JobView;
  /** 2 Hz, while the job is running. Every number the window puts on screen. */
  summary = $state.raw<SummaryFrame | null>(null);
  /** 20 Hz, and only while this row is the expanded one. The map, and nothing else. */
  detail = $state.raw<ProgressFrame | null>(null);

  constructor(view: JobView) {
    this.view = view;
  }

  get id(): JobId {
    return this.view.id;
  }

  /**
   * The numbers this row shows — the summary, always, expanded or not.
   *
   * Not the detail frame, even while one is arriving. A map is a picture and may move at
   * the speed of the thing it draws; a speed, an ETA and a percentage are *read*, and a
   * figure that changes twenty times a second cannot be. Opening a row must not turn its
   * readouts into a blur while the identical row below it stays legible
   * (05 §Numbers that don't lie or twitch, and 20 Hz is the map's cadence in §Rendering).
   */
  get frame(): SummaryFrame | null {
    return this.summary;
  }

  get completed(): number {
    return this.frame?.completed ?? this.view.completed;
  }

  get bps(): number {
    return this.view.state.kind === "downloading" ? (this.frame?.bps ?? 0) : 0;
  }
}

/**
 * What the filter bar narrows the list to.
 *
 * There used to be a third arm here — `{ by: "state" }`, with Active, Queued and Done as
 * three entries in a left-hand column. They were never really three lists: a job passes
 * through all three in the course of one download, so the column was asking the user to
 * follow their own file between sections and to keep checking a *different* section for
 * the answer to "did it finish". One list in state order says the same thing without the
 * navigation, which is most of why there is no column any more (05 §Layout).
 */
export type Filter = { by: "all" } | { by: "category"; category: Category };

/**
 * Where a job sits in the one list.
 *
 * The same three words the old column used, doing the job they were always doing — but as an
 * *ordering* rather than a filter, which is what they were always closer to. Read as a
 * sentence, top to bottom: what is happening now, what is waiting to, and what already
 * did.
 */
export type Band = "active" | "queued" | "done";

/** Top to bottom. The one place the order of the list is written down. */
export const BANDS: Band[] = ["active", "queued", "done"];

export function band(state: JobState): Band {
  return isDone(state) ? "done" : state.kind === "queued" ? "queued" : "active";
}

export function isActive(state: JobState): boolean {
  return !isDone(state) && state.kind !== "queued";
}

/**
 * Every `Category`, in the order the filter bar draws them (05 §Layout).
 *
 * Written out because a TypeScript union cannot be enumerated at runtime, and the proto's
 * own declaration order is the display order — `crates/vortex-proto/src/lib.rs` says so
 * where `Category` is defined, and this has to be kept beside it.
 */
const KINDS: Category[] = [
  "Video",
  "Audio",
  "Archives",
  "Documents",
  "Images",
  "Programs",
  "Other",
];

/**
 * How many rows the list renders before it is asked for more.
 *
 * Forty, because forty is the number the quality floor is written against — 60 fps with
 * forty jobs listed (05 §Quality floor) — and because the list is deliberately not
 * virtualised (`JobList.svelte`). While Done was its own section in the column that was the
 * whole story: nobody had a hundred *active* downloads. One list ends with every finished
 * job ever, so the cap moved from being a property of how people use the app to being
 * something the list has to enforce.
 */
const PAGE = 40;

/** The states in which the app is doing something on the user's behalf. See `Queue.working`. */
const WORKING = new Set<JobState["kind"]>(["probing", "downloading", "muxing"]);

export function isDone(state: JobState): boolean {
  return state.kind === "completed" || state.kind === "failed";
}

/** A job the user is being asked about, or one that stopped and said why. */
export function needsAttention(state: JobState): boolean {
  return state.kind === "needsDecision" || state.kind === "failed";
}

class Queue {
  /** Ordered newest first. Rows never reorder themselves while you are looking at them. */
  jobs = $state.raw<Job[]>([]);
  settings = $state.raw<Settings | null>(null);
  connected = $state(false);
  /** The daemon's own words, shown once and dismissed. Never a code, never a stack. */
  problem = $state<string | null>(null);
  /**
   * The answer to the last `Probe`, which is what the New Download sheet renders. It lives
   * here rather than in the sheet so there is one subscriber to the event stream: a second
   * `onEvent` listener is a second thing to unsubscribe and a second place for the order of
   * two events to matter.
   */
  probed = $state.raw<ProbeResult | null>(null);

  /**
   * How many of `visible` the list is currently rendering. See {@link PAGE}.
   *
   * Public because the list grows it as the user reaches the bottom and `reveal` grows it
   * to reach a row, and both of those are "show more of what is already here" rather than
   * a fetch — every job is already in this process. There is no loading state to draw
   * because there is nothing to wait for.
   */
  shown = $state(PAGE);

  /**
   * The filter and the search, behind accessors for one reason: **narrowing the list has
   * to put the page back to the top of it.** Type a search that matches one job and the
   * page count from the last scroll would otherwise still be sitting at 200, so the next
   * `Show more` would appear to do nothing and the count in the footer would be wrong.
   */
  #filter = $state.raw<Filter>({ by: "all" });
  #search = $state("");

  get filter(): Filter {
    return this.#filter;
  }

  set filter(next: Filter) {
    this.#filter = next;
    this.shown = PAGE;
    this.replaced();
  }

  /** Ctrl-F. Empty means no filtering, not "match nothing". */
  get search(): string {
    return this.#search;
  }

  set search(next: string) {
    this.#search = next;
    this.shown = PAGE;
    this.replaced();
  }

  /**
   * Whether the list should animate the rows it is about to gain and lose.
   *
   * A row folding open is a download arriving and a row folding away is one leaving, and
   * both are worth 180 ms because both are *one thing happening* — the motion says which
   * row, and where the space went. Changing the filter is not one thing happening. It is
   * forty rows leaving and forty arriving at once, and choreographing that is a screenful
   * of movement that says nothing, arrives late, and animates height forty times in a
   * frame. Same for a page appended at the bottom, which the user asked for by scrolling
   * and does not need announced, and for the daemon's opening `Listed`, which is not an
   * arrival at all — those rows were already downloading before this window existed.
   *
   * So the four places that replace the list wholesale lower this for exactly one flush,
   * and a row reads it at the moment its transition would start. It is a property of the
   * *change*, not of the row: the same row is worth animating in one context and not in
   * the other, and only the thing making the change knows which.
   */
  animating = $state(true);

  /**
   * "What follows is not one thing happening." Raised again once the DOM has caught up.
   *
   * `tick` rather than a timer, because the window between lowering and raising this has
   * to be exactly the flush that renders the change — a millisecond too short and the
   * transitions run anyway, a millisecond too long and the next genuine arrival is silent.
   */
  private replaced(): void {
    this.animating = false;
    void tick().then(() => (this.animating = true));
  }

  selected = $state<JobId | null>(null);
  expanded = $state<JobId | null>(null);
  /**
   * The job the Remove sheet is asking about, if it is open. It lives here for the same
   * reason `expanded` does: two places ask to remove a job — the row's × and the Delete
   * key — and they must open the same one sheet rather than each own a copy.
   */
  removing = $state<JobId | null>(null);

  #index = new Map<JobId, Job>();

  get(id: JobId): Job | undefined {
    return this.#index.get(id);
  }

  /**
   * The filter bar's numbers, over the whole queue rather than over what is visible.
   *
   * There were three more here — one per state — back when the states were their own entries.
   * They are counted where they are now drawn instead, in `sections`, which is over
   * `visible` rather than over everything: a heading describes the rows under it, and a
   * chip describes what is behind it.
   */
  /**
   * The kinds the filter bar offers, in the proto's order.
   *
   * **What exists, not a fixed set with zeros beside it.** The old left column always drew
   * Video, Audio, Archives and Documents so that it had a stable shape; a *row* of chips
   * reading `Video 0 · Audio 0 · Archives 0` is four pieces of furniture saying nothing,
   * and across the top there is nowhere for them to hide.
   *
   * The exception is whichever kind is selected, which stays even after its last job is
   * removed. Otherwise the chip vanishes under the pointer that just clicked it and the
   * list is left filtered by something that is no longer on screen — with no way back to
   * All except a control that has moved.
   */
  kinds = $derived.by(() => {
    const filter = this.filter;
    return KINDS.filter(
      (kind) =>
        (this.byCategory.get(kind) ?? 0) > 0 ||
        (filter.by === "category" && filter.category === kind),
    );
  });

  byCategory = $derived.by(() => {
    const counts = new Map<Category, number>();
    for (const job of this.jobs) {
      const category = job.view.category;
      counts.set(category, (counts.get(category) ?? 0) + 1);
    }
    return counts;
  });

  /**
   * The whole list the user has asked for: the filter, then the search, then state order.
   *
   * Ordered in three bands — active, queued, done — and newest first inside each, which
   * falls out of `jobs` already being sorted and this being a stable partition rather than
   * a sort.
   *
   * **This is the one place a row moves on its own**, and it is worth being explicit about
   * because `jobs` promises the opposite. A job that finishes leaves the top band and
   * appears in the bottom one; a queued job that starts goes the other way. That is a
   * reorder while somebody is looking at the list — but it is the only kind that is
   * legible, because the row moved at the exact moment its state changed and the heading
   * it moved under says which state. The alternative is a list in creation order where a
   * download that finished an hour ago sits above one that is running, which is how a
   * queue stops being readable.
   */
  visible = $derived.by(() => {
    const filter = this.filter;
    const needle = this.search.trim().toLowerCase();
    const bands: Record<Band, Job[]> = { active: [], queued: [], done: [] };
    for (const job of this.jobs) {
      if (filter.by === "category" && job.view.category !== filter.category) continue;
      if (
        needle &&
        !job.view.filename.toLowerCase().includes(needle) &&
        !job.view.host.toLowerCase().includes(needle)
      ) {
        continue;
      }
      bands[band(job.view.state)].push(job);
    }
    return [...bands.active, ...bands.queued, ...bands.done];
  });

  /** How many of `visible` are not being rendered yet. Zero is the ordinary case. */
  get rest(): number {
    return Math.max(0, this.visible.length - this.shown);
  }

  /** What the list actually renders (`shown` of `visible`), still in band order. */
  rows = $derived(this.visible.slice(0, this.shown));

  /**
   * The rendered rows cut into their headed sections.
   *
   * `total` counts the band across the whole of `visible`, not the part being rendered, so
   * the heading says *Done 412* while twelve of them are on screen. A heading that counted
   * only what had been paged in would be a number that grew as the user scrolled, which is
   * the one thing a count must never do.
   *
   * A band with nothing in it has no heading. Three empty sections stacked above an empty
   * state is an interface explaining its own internals.
   */
  sections = $derived.by(() => {
    const totals: Record<Band, number> = { active: 0, queued: 0, done: 0 };
    for (const job of this.visible) totals[band(job.view.state)] += 1;
    const members: Record<Band, Job[]> = { active: [], queued: [], done: [] };
    for (const job of this.rows) members[band(job.view.state)].push(job);
    return BANDS.filter((name) => totals[name] > 0).map((name) => ({
      band: name,
      total: totals[name],
      jobs: members[name],
    }));
  });

  /** Renders another page. Called by the list's sentinel, and by `reveal` reaching a row. */
  more(): void {
    if (this.rest <= 0) return;
    this.shown += PAGE;
    this.replaced();
  }

  /**
   * The one number that is true of the whole app, and the reason the trace lives in the
   * titlebar rather than in a row (05 §Layout).
   */
  throughput = $derived(this.jobs.reduce((sum, job) => sum + job.bps, 0));

  /** Everything that is moving, for the tray tooltip and the taskbar. */
  running = $derived(this.jobs.filter((job) => job.view.state.kind === "downloading").length);

  /**
   * Everything genuinely working, as opposed to merely unfinished — the question the
   * taskbar asks.
   *
   * Wider than `running`, because a job that is probing or muxing is doing something the
   * user is waiting on even though no bytes are landing. Narrower than the list's
   * "active" band, because paused, stalled and needs-an-answer jobs are standing still, and a
   * taskbar that animates for a queue nothing is happening to is a lie the shell tells on
   * the app's behalf.
   */
  working = $derived(
    this.jobs.filter((job) => WORKING.has(job.view.state.kind)).length,
  );

  /**
   * How far along the whole queue is, for the taskbar progress bar. Jobs of unknown size
   * are left out of both halves rather than counted as zero — a 4 GB download beside a
   * chunked stream should not read as 50% because one of them cannot be measured.
   */
  progress = $derived.by(() => {
    let done = 0;
    let total = 0;
    for (const job of this.jobs) {
      if (!isActive(job.view.state) || !job.view.total) continue;
      done += job.completed;
      total += job.view.total;
    }
    return total > 0 ? done / total : null;
  });

  apply(event: Event): void {
    switch (event.event) {
      case "jobs":
        this.#reset(event.jobs);
        // The queue as it already was. Not forty arrivals — see `animating`.
        this.replaced();
        break;
      case "jobAdded":
        this.#upsert(event.job);
        break;
      case "jobRemoved":
        this.#remove(event.job);
        break;
      case "jobStateChanged": {
        const job = this.#index.get(event.job);
        if (job) job.view = { ...job.view, state: event.state };
        break;
      }
      case "jobFinished": {
        const job = this.#index.get(event.job);
        if (job && event.outcome.kind === "completed") {
          job.view = {
            ...job.view,
            completed: event.outcome.bytes,
            verified: event.outcome.verified ?? job.view.verified,
          };
          job.summary = null;
          job.detail = null;
        }
        break;
      }
      case "jobSummary": {
        const job = this.#index.get(event.job);
        if (job) job.summary = event.frame;
        break;
      }
      case "jobProgress": {
        // Only the expanded row may hold a detail frame, and this is the second half of
        // that rule — `expand` is the first. A frame for a row that has just been collapsed
        // was already in flight when the subscription moved, and storing it would leave
        // that row holding a frame nothing will ever replace, because the daemon has
        // stopped sending it any.
        if (event.job !== this.expanded) break;
        const job = this.#index.get(event.job);
        if (job) job.detail = event.frame;
        break;
      }
      case "probed":
        this.probed = event.result;
        break;
      case "settingsChanged":
        this.settings = event.settings;
        break;
      case "error":
        this.problem = event.message;
        break;
      default:
        // `hello`, `pong`, `mediaFound` and `urlExpired` are addressed to somebody else —
        // the last two are the extension's business and reach this process only because
        // events are broadcast. Ignoring them here is correct.
        break;
    }
  }

  #reset(views: JobView[]): void {
    this.#index = new Map(views.map((view) => [view.id, new Job(view)]));
    this.jobs = this.#sorted();
    settle(true);
  }

  #upsert(view: JobView): void {
    const existing = this.#index.get(view.id);
    if (existing) {
      existing.view = view;
      return;
    }
    this.#index.set(view.id, new Job(view));
    this.jobs = this.#sorted();
    settle();
  }

  #remove(id: JobId): void {
    if (!this.#index.delete(id)) return;
    if (this.selected === id) this.selected = null;
    if (this.expanded === id) this.expanded = null;
    // The CLI, or a second window, can remove the job this sheet is asking about. Asking
    // about a row that is already gone is a question with nothing behind it.
    if (this.removing === id) this.removing = null;
    this.jobs = this.#sorted();
  }

  #sorted(): Job[] {
    // Newest first, and by id when two jobs were created in the same second — which is
    // what a batch handed over from the browser looks like.
    return [...this.#index.values()].sort(
      (a, b) => b.view.createdAt - a.view.createdAt || Number(b.id) - Number(a.id),
    );
  }
}

export const queue = new Queue();

/**
 * Opens or closes the expanded row.
 *
 * The 20 Hz frames only exist while somebody is looking (01 §IPC), so this is also the
 * subscription: expanding asks the daemon to start sending them and collapsing asks it to
 * stop. Leaving that to a component's lifecycle would mean a row that unmounts during a
 * filter change keeps the daemon talking to nobody.
 */
export async function expand(id: JobId | null): Promise<void> {
  const next = queue.expanded === id ? null : id;
  // The row being left behind gives its detail frame up, because the daemon is about to
  // stop sending it any. A frame kept past that point is a map frozen mid-download — and
  // the one it would be showing next time the row is opened, before the first live frame
  // lands, is a picture of where the file was some minutes ago.
  const previous = queue.expanded === null ? null : queue.get(queue.expanded);
  if (previous) previous.detail = null;
  queue.expanded = next;
  const job = next === null ? null : queue.get(next);
  if (job) job.detail = null;
  await daemon.watch(next);
}

/**
 * A job somebody asked for before the list had it.
 *
 * `vortex-app --reveal 42` is the case: the window is started *by* the request, so the
 * frontend reads it on mount and the queue is still empty — the daemon's answer to `list`
 * is one round trip behind. Dropping the id there would mean a cold click from the browser
 * opens a window that looks exactly like an ordinary one, which is the failure the whole
 * path exists to avoid.
 *
 * One slot, not a queue. A second request supersedes the first: nobody wants two rows
 * opened, and the last thing asked for is the thing being waited on.
 */
let waiting: JobId | null = null;

/**
 * Retries a reveal that arrived before its job did. Called wherever a job appears.
 *
 * `whole` says the caller has just replaced the entire list, which is the authoritative
 * answer to "is there such a job": if it is not in there, no later event will produce it,
 * and an id kept past that point is a reveal armed to fire on something else.
 */
function settle(whole = false): void {
  if (waiting === null) return;
  if (queue.get(waiting)) void reveal(waiting);
  else if (whole) waiting = null;
}

/**
 * Brings one job into view — a notification's click, the extension's `Reveal`, and the
 * only thing that reaches into the list from outside it.
 *
 * Two things can be hiding the row, and both have to be cleared or the window looks like
 * it ignored the click: a *filter* it does not match, and a *page* it falls past. The
 * second is new and is the less obvious one — a finished download from last week is in the
 * list, in the right place, four hundred rows down, and a `selected` pointing at a row
 * nothing has rendered is a cursor on nothing at all.
 *
 * Neither is touched unless it has to be. Somebody mid-way through narrowing the list did
 * not ask for it to be reset, and a notification about a job they can already see is not a
 * reason to.
 */
export async function reveal(id: JobId): Promise<void> {
  const job = queue.get(id);
  if (!job) {
    waiting = id;
    return;
  }
  waiting = null;
  if (!queue.visible.some((row) => row.id === id)) {
    // Both, and in this order: `search`'s setter puts the page back to the top, and a
    // category the job is not in would still be hiding it afterwards.
    queue.search = "";
    queue.filter = { by: "all" };
  }
  // Whole pages rather than exactly enough, so the row lands inside a page instead of on
  // its last line with nothing under it.
  const at = queue.visible.findIndex((row) => row.id === id);
  while (at >= queue.shown && queue.rest > 0) queue.more();
  queue.selected = id;
  if (queue.expanded !== id) await expand(id);
}
