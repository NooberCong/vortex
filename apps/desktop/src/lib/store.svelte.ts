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
  /** 2 Hz, while the job is running. Everything a collapsed row needs. */
  summary = $state.raw<SummaryFrame | null>(null);
  /** 20 Hz, and only while this row is the expanded one. */
  detail = $state.raw<ProgressFrame | null>(null);

  constructor(view: JobView) {
    this.view = view;
  }

  get id(): JobId {
    return this.view.id;
  }

  /** The most recent frame, whichever cadence it came from. */
  get frame(): ProgressFrame | SummaryFrame | null {
    return this.detail ?? this.summary;
  }

  get completed(): number {
    return this.frame?.completed ?? this.view.completed;
  }

  get bps(): number {
    return this.view.state.kind === "downloading" ? (this.frame?.bps ?? 0) : 0;
  }
}

/** The sidebar's sections. `state` and `kind` are different questions about the same list. */
export type Filter =
  | { by: "state"; state: "active" | "queued" | "done" }
  | { by: "category"; category: Category };

export function isActive(state: JobState): boolean {
  return !isDone(state) && state.kind !== "queued";
}

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

  filter = $state.raw<Filter>({ by: "state", state: "active" });
  /** Ctrl-F. Empty means no filtering, not "match nothing". */
  search = $state("");
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

  counts = $derived.by(() => {
    const counts = {
      active: 0,
      queued: 0,
      done: 0,
      byCategory: new Map<Category, number>(),
    };
    for (const job of this.jobs) {
      const state = job.view.state;
      if (isDone(state)) counts.done += 1;
      else if (state.kind === "queued") counts.queued += 1;
      else counts.active += 1;
      const category = job.view.category;
      counts.byCategory.set(category, (counts.byCategory.get(category) ?? 0) + 1);
    }
    return counts;
  });

  /** What the list renders: the filter, then the search, in that order. */
  visible = $derived.by(() => {
    const filter = this.filter;
    const needle = this.search.trim().toLowerCase();
    return this.jobs.filter((job) => {
      const state = job.view.state;
      const matchesFilter =
        filter.by === "category"
          ? job.view.category === filter.category
          : filter.state === "done"
            ? isDone(state)
            : filter.state === "queued"
              ? state.kind === "queued"
              : isActive(state);
      if (!matchesFilter) return false;
      if (!needle) return true;
      return (
        job.view.filename.toLowerCase().includes(needle) ||
        job.view.host.toLowerCase().includes(needle)
      );
    });
  });

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
   * user is waiting on even though no bytes are landing. Narrower than the sidebar's
   * "active", because paused, stalled and needs-an-answer jobs are standing still, and a
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
  }

  #upsert(view: JobView): void {
    const existing = this.#index.get(view.id);
    if (existing) {
      existing.view = view;
      return;
    }
    this.#index.set(view.id, new Job(view));
    this.jobs = this.#sorted();
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
  queue.expanded = next;
  const job = next === null ? null : queue.get(next);
  if (job) job.detail = null;
  await daemon.watch(next);
}

/**
 * Brings one job into view — the notification's click, and the only thing that reaches
 * into the list from outside it.
 *
 * The filter is half the answer. A finished job is not in the default "active" view, so
 * selecting it there would move a cursor onto a row that is not on screen and never will
 * be: the user clicks a toast about a download and gets a window that looks unchanged.
 * The filter only moves when it has to, because a user who was mid-way through narrowing
 * the list did not ask for it to be reset.
 */
export async function reveal(id: JobId): Promise<void> {
  const job = queue.get(id);
  if (!job) return;
  if (!queue.visible.some((row) => row.id === id)) {
    const state = job.view.state;
    queue.search = "";
    queue.filter = {
      by: "state",
      state: isDone(state) ? "done" : state.kind === "queued" ? "queued" : "active",
    };
  }
  queue.selected = id;
  if (queue.expanded !== id) await expand(id);
}
