import { tick } from "svelte";
import { beforeEach, describe, expect, it } from "vitest";

import type { Event, JobId, JobState, JobView, ProgressFrame, SummaryFrame } from "@vortex/proto";

import { expand, isActive, isDone, needsAttention, queue, reveal } from "$lib/store.svelte";

/**
 * The window's model of the queue.
 *
 * Everything here is a projection of the event stream, so the tests are event streams. The
 * one property worth pinning hardest is that a job's numbers come from its frame while it
 * is running and from its view when it is not — an app that shows a stale 0% next to a
 * paused job that is 28% done has told the user something false about their file.
 */

let next = 1;

function view(over: Partial<JobView> = {}): JobView {
  const id = (over.id ?? next++) as JobId;
  return {
    id,
    filename: `file-${id}.iso`,
    destPath: `D:\\Downloads\\file-${id}.iso`,
    url: `https://example.com/file-${id}.iso`,
    host: "example.com",
    category: "Archives",
    state: { kind: "downloading" },
    priority: "Normal",
    total: 1000,
    completed: 0,
    mode: "Parallel",
    protocol: "H2",
    addresses: 1,
    createdAt: 1_700_000_000 + Number(id),
    finishedAt: null,
    verified: null,
    retries: { total: 0, recovered: 0 },
    media: null,
    ...over,
  };
}

const frame = (over: Partial<SummaryFrame> = {}): SummaryFrame => ({
  total: 1000,
  completed: 250,
  bps: 2_000_000,
  etaSecs: 30,
  connections: 4,
  blocks: 100,
  runs: [[0, 25, 0]],
  ...over,
});

/** What the expanded row is sent, twenty times a second. */
const live = (over: Partial<ProgressFrame> = {}): ProgressFrame => ({
  ...frame(),
  blockSize: 1 << 20,
  workers: [],
  ...over,
});

const feed = (...events: Event[]) => events.forEach((e) => queue.apply(e));

beforeEach(() => {
  next = 1;
  feed({ event: "jobs", jobs: [] });
  queue.filter = { by: "all" };
  queue.search = "";
  queue.shown = 40;
  queue.selected = null;
  queue.expanded = null;
  queue.animating = true;
});

describe("applying events", () => {
  it("replaces the whole list on `jobs`, because a reconnect is a repaint", () => {
    // There is no delta to apply: jobs may have finished, failed or been added while the
    // app was not listening.
    feed({ event: "jobs", jobs: [view(), view()] });
    expect(queue.jobs).toHaveLength(2);
    feed({ event: "jobs", jobs: [view({ id: 9 as JobId })] });
    expect(queue.jobs.map((j) => j.id)).toEqual([9]);
  });

  it("orders newest first and does not reorder while you are looking", () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId }), view({ id: 2 as JobId })] });
    expect(queue.jobs.map((j) => j.id)).toEqual([2, 1]);
    // A frame arriving for the older job must not lift it to the top.
    feed({ event: "jobSummary", job: 1 as JobId, frame: frame() });
    expect(queue.jobs.map((j) => j.id)).toEqual([2, 1]);
  });

  it("takes live numbers from the frame and resting numbers from the view", () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId, completed: 10 })] });
    const job = queue.get(1 as JobId)!;
    expect(job.completed).toBe(10);

    feed({ event: "jobSummary", job: 1 as JobId, frame: frame({ completed: 250 }) });
    expect(job.completed).toBe(250);
    expect(job.bps).toBe(2_000_000);

    // Paused: the frame stops arriving, and the last one it sent is still the truth about
    // how far the file got.
    feed({ event: "jobStateChanged", job: 1 as JobId, state: { kind: "paused" } });
    expect(job.completed).toBe(250);
    expect(job.bps, "a paused job moves at nothing, whatever its last frame said").toBe(0);
  });

  it("forgets the frames of a finished job", () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId })] });
    feed({ event: "jobSummary", job: 1 as JobId, frame: frame() });
    feed({
      event: "jobFinished",
      job: 1 as JobId,
      outcome: {
        kind: "completed",
        path: "D:\\x",
        bytes: 1000,
        elapsedSecs: 12,
        verified: { algorithm: "SHA-256", ok: true },
      },
    });
    const job = queue.get(1 as JobId)!;
    expect(job.summary).toBeNull();
    expect(job.completed).toBe(1000);
    expect(job.view.verified).toEqual({ algorithm: "SHA-256", ok: true });
  });

  it("clears the selection when the selected job is removed", () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId })] });
    queue.selected = 1 as JobId;
    queue.expanded = 1 as JobId;
    feed({ event: "jobRemoved", job: 1 as JobId });
    expect(queue.selected).toBeNull();
    expect(queue.expanded).toBeNull();
  });

  it("passes an event addressed to somebody else through untouched", () => {
    feed({ event: "jobs", jobs: [view()] });
    feed({ event: "pong" }, { event: "mediaFound", tab: 4 as never, candidates: [] });
    expect(queue.jobs).toHaveLength(1);
    expect(queue.problem).toBeNull();
  });
});

describe("the list's arithmetic", () => {
  const states: JobState[] = [
    { kind: "downloading" },
    { kind: "paused" },
    { kind: "queued" },
    { kind: "completed" },
    { kind: "failed", error: "x" },
  ];

  it("puts every job in exactly one band", () => {
    // The bands are the whole structure of the list. If they do not add up, the user has a
    // download that is in the queue and under no heading.
    feed({ event: "jobs", jobs: states.map((state) => view({ state })) });
    const totals = Object.fromEntries(queue.sections.map((s) => [s.band, s.total]));
    expect(queue.sections.reduce((n, s) => n + s.total, 0)).toBe(states.length);
    expect(totals).toEqual({ active: 2, queued: 1, done: 2 });
  });

  it("offers only the kinds that exist, in the proto's order", () => {
    // The left column always drew Video, Audio, Archives and Documents so it had a stable
    // shape. A *row* of chips reading `Video 0 · Audio 0 · Archives 0` is furniture.
    feed({
      event: "jobs",
      jobs: [
        view({ id: 1 as JobId, category: "Programs" }),
        view({ id: 2 as JobId, category: "Video" }),
      ],
    });
    expect(queue.kinds).toEqual(["Video", "Programs"]);
  });

  it("keeps the selected kind even after its last job goes", () => {
    // Otherwise the chip vanishes under the pointer that clicked it and the list is left
    // filtered by something no longer on screen, with no way back to All.
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId, category: "Audio" })] });
    queue.filter = { by: "category", category: "Audio" };
    feed({ event: "jobRemoved", job: 1 as JobId });

    expect(queue.kinds).toEqual(["Audio"]);
    expect(queue.byCategory.get("Audio") ?? 0).toBe(0);
  });

  it("counts every job under exactly one category", () => {
    feed({ event: "jobs", jobs: states.map((state) => view({ state })) });
    expect([...queue.byCategory.values()].reduce((a, b) => a + b, 0)).toBe(states.length);
  });

  it("agrees with the predicates the rows use", () => {
    for (const state of states) {
      expect(Number(isActive(state)) + Number(isDone(state)) + Number(state.kind === "queued")).toBe(
        1,
      );
    }
    expect(needsAttention({ kind: "failed", error: "x" })).toBe(true);
    expect(needsAttention({ kind: "downloading" })).toBe(false);
  });
});

describe("what the list shows", () => {
  beforeEach(() => {
    feed({
      event: "jobs",
      jobs: [
        view({ id: 1 as JobId, filename: "ubuntu.iso", category: "Archives" }),
        view({ id: 2 as JobId, filename: "lecture.mp4", category: "Video", host: "cdn.edu" }),
        view({ id: 3 as JobId, state: { kind: "completed" }, category: "Video" }),
      ],
    });
  });

  it("puts what is happening above what already happened", () => {
    // The whole point of one list: active first, then queued, then done, newest first
    // inside each band. Job 3 is completed, so it goes last however new it is.
    expect(queue.visible.map((j) => j.id)).toEqual([2, 1, 3]);
  });

  it("moves a row to its band the moment its state changes", () => {
    // The one reorder the list allows itself, and the one a user can follow: the row moved
    // because the thing it describes changed, and the heading it moved under says so.
    feed({ event: "jobStateChanged", job: 2 as JobId, state: { kind: "completed" } });
    expect(queue.visible.map((j) => j.id)).toEqual([1, 3, 2]);
  });

  it("searches the host as well as the filename", () => {
    queue.search = "cdn.edu";
    expect(queue.visible.map((j) => j.id)).toEqual([2]);
  });

  it("treats an empty search as no filtering, not as matching nothing", () => {
    queue.search = "   ";
    expect(queue.visible).toHaveLength(3);
  });

  it("filters by category across every state", () => {
    queue.filter = { by: "category", category: "Video" };
    expect(queue.visible.map((j) => j.id)).toEqual([2, 3]);
  });

  it("heads only the bands that have something in them", () => {
    // Three empty sections stacked over an empty state is an interface explaining its own
    // internals. Nothing is queued here, so there is no Queued heading.
    expect(queue.sections.map((s) => [s.band, s.total])).toEqual([
      ["active", 2],
      ["done", 1],
    ]);
  });
});

describe("how much of the list is drawn", () => {
  /**
   * The list is not virtualised, so it renders a page at a time (`JobList.svelte`). While
   * Done was its own filter this never mattered — nobody has two hundred *active*
   * downloads. One list ends with every download ever finished.
   */
  const lots = (count: number) =>
    feed({
      event: "jobs",
      jobs: Array.from({ length: count }, (_, i) =>
        view({ id: (200 + i) as JobId, state: { kind: "completed" } }),
      ),
    });

  it("draws a page, and says how much it is not drawing", () => {
    lots(100);
    expect(queue.rows).toHaveLength(40);
    expect(queue.rest).toBe(60);
  });

  it("counts the whole band in the heading, not the part on screen", () => {
    // A heading that counted only what had been paged in would be a number that grew as
    // the user scrolled, which is the one thing a count must never do.
    lots(100);
    expect(queue.sections).toEqual([
      expect.objectContaining({ band: "done", total: 100 }),
    ]);
    expect(queue.sections[0]?.jobs).toHaveLength(40);
  });

  it("stops asking once everything is drawn", () => {
    lots(45);
    queue.more();
    expect(queue.rest).toBe(0);
    queue.more();
    queue.more();
    expect(queue.rows).toHaveLength(45);
  });

  it("goes back to the top of the list when the list is narrowed", () => {
    // Type a search that matches one job and a page count left over from the last scroll
    // would still be sitting at two hundred: the footer count would be wrong and the next
    // `more` would appear to do nothing.
    lots(100);
    queue.more();
    expect(queue.shown).toBe(80);

    queue.search = "file-2";
    expect(queue.shown).toBe(40);

    queue.search = "";
    queue.filter = { by: "category", category: "Video" };
    expect(queue.shown).toBe(40);
  });
});

describe("the aggregate", () => {
  it("counts only what is moving", () => {
    feed({
      event: "jobs",
      jobs: [view({ id: 1 as JobId }), view({ id: 2 as JobId, state: { kind: "paused" } })],
    });
    feed({ event: "jobSummary", job: 1 as JobId, frame: frame({ bps: 3_000_000 }) });
    feed({ event: "jobSummary", job: 2 as JobId, frame: frame({ bps: 9_000_000 }) });
    expect(queue.throughput).toBe(3_000_000);
    expect(queue.running).toBe(1);
  });

  it("leaves a job of unknown size out of the taskbar total entirely", () => {
    // A 4 GB download beside a chunked stream must not read as 50% because one of them
    // cannot be measured.
    feed({
      event: "jobs",
      jobs: [
        view({ id: 1 as JobId, total: 1000, completed: 500 }),
        view({ id: 2 as JobId, total: null, completed: 0 }),
      ],
    });
    expect(queue.progress).toBeCloseTo(0.5, 5);
  });

  it("has no progress to report when nothing measurable is running", () => {
    feed({ event: "jobs", jobs: [view({ state: { kind: "completed" } })] });
    expect(queue.progress).toBeNull();
  });

  it("counts probing and muxing as working, because the user is waiting on both", () => {
    feed({
      event: "jobs",
      jobs: [
        view({ id: 1 as JobId, state: { kind: "probing" } }),
        view({ id: 2 as JobId, state: { kind: "downloading" } }),
        view({ id: 3 as JobId, state: { kind: "muxing" } }),
      ],
    });
    expect(queue.working).toBe(3);
    // None of them is a download in flight; only one is.
    expect(queue.running).toBe(1);
  });

  it("does not call a queue standing still a working one", () => {
    // Each of these is "active" in the list's sense — unfinished, not queued — and
    // nothing is happening to any of them. A taskbar that animates here is a lie.
    feed({
      event: "jobs",
      jobs: [
        view({ id: 1 as JobId, state: { kind: "paused" } }),
        view({ id: 2 as JobId, state: { kind: "stalled", reason: "no route to host" } }),
        view({ id: 3 as JobId, state: { kind: "queued" } }),
      ],
    });
    expect(queue.working).toBe(0);
  });
});

describe("opening and closing a row", () => {
  /**
   * Opening a row changes what is drawn, never what is read.
   *
   * The 20 Hz frame is for the map — a picture, which may move at the speed of the thing it
   * draws. The speed, ETA and percentage beside it stay on the 2 Hz summary, because a
   * figure that changes twenty times a second is a figure nobody can read
   * (05 §Numbers that don't lie or twitch).
   */
  it("keeps the row's numbers on the 2 Hz summary while the map runs at 20 Hz", async () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId })] });
    const job = queue.get(1 as JobId)!;
    feed({ event: "jobSummary", job: 1 as JobId, frame: frame({ bps: 2_000 }) });

    await expand(1 as JobId);
    feed({ event: "jobProgress", job: 1 as JobId, frame: live({ bps: 5_000, blocks: 64 }) });

    expect(job.bps, "opening the row put the readout on the 20 Hz stream").toBe(2_000);
    expect(job.detail?.blocks, "the map is not getting its frames").toBe(64);
  });

  /**
   * And the other half: only the expanded row may hold a detail frame at all. A row that
   * kept one after the subscription moved on would be holding a frame nothing will ever
   * replace — the daemon has stopped sending it any — which is a map frozen mid-download
   * under numbers that are still moving.
   */
  it("hands the detail frame over when another row is opened", async () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId }), view({ id: 2 as JobId })] });
    const first = queue.get(1 as JobId)!;

    await expand(1 as JobId);
    feed({ event: "jobProgress", job: 1 as JobId, frame: live() });
    await expand(2 as JobId);

    expect(first.detail).toBeNull();
  });

  it("gives it up when the row is closed", async () => {
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId })] });
    const job = queue.get(1 as JobId)!;

    await expand(1 as JobId);
    feed({ event: "jobProgress", job: 1 as JobId, frame: live() });
    await expand(1 as JobId);

    expect(job.detail).toBeNull();
  });

  it("drops a detail frame that was already in flight when the row closed", async () => {
    // The subscription moves at the speed of a round trip; the frames do not stop dead.
    // One that lands after the row is closed would put the row straight back into the
    // state the two tests above are about, and nothing would ever clear it again.
    feed({ event: "jobs", jobs: [view({ id: 1 as JobId })] });
    const job = queue.get(1 as JobId)!;

    await expand(1 as JobId);
    await expand(1 as JobId);
    feed({ event: "jobProgress", job: 1 as JobId, frame: live() });

    expect(job.detail).toBeNull();
  });
});

describe("bringing one job into view", () => {
  it("clears a category that was hiding the job", async () => {
    // The click on a "download finished" notification. Selecting a row nothing is drawing
    // is a cursor on something that is not on screen: a window that looks like it ignored
    // the click.
    feed({
      event: "jobs",
      jobs: [
        view({ id: 1 as JobId, category: "Video" }),
        view({ id: 2 as JobId, state: { kind: "completed" }, category: "Archives" }),
      ],
    });
    queue.filter = { by: "category", category: "Video" };
    expect(queue.visible.map((job) => job.id)).toEqual([1]);

    await reveal(2 as JobId);
    expect(queue.filter).toEqual({ by: "all" });
    expect(queue.selected).toBe(2);
    expect(queue.expanded, "the row opens, because that is what was clicked").toBe(2);
  });

  it("finds a failed job too, which is the half of `done` nobody looks for", async () => {
    feed({
      event: "jobs",
      jobs: [view({ id: 3 as JobId, state: { kind: "failed", error: "The server hung up." } })],
    });
    await reveal(3 as JobId);
    expect(queue.visible.map((job) => job.id)).toEqual([3]);
    expect(queue.selected).toBe(3);
  });

  it("clears a search that was hiding the job", async () => {
    feed({ event: "jobs", jobs: [view({ id: 4 as JobId, state: { kind: "completed" } })] });
    queue.search = "something else";
    await reveal(4 as JobId);
    expect(queue.search).toBe("");
    expect(queue.visible.map((job) => job.id)).toEqual([4]);
  });

  it("pages down to a job that is past the end of what is rendered", async () => {
    // A finished download from last week: in the list, in the right band, four hundred
    // rows down. The filter is not what is hiding it — the page is.
    const many = Array.from({ length: 120 }, (_, i) =>
      view({ id: (100 + i) as JobId, state: { kind: "completed" } }),
    );
    feed({ event: "jobs", jobs: many });
    const last = queue.visible[queue.visible.length - 1]!;
    expect(queue.rows.some((row) => row.id === last.id), "not rendered yet").toBe(false);

    await reveal(last.id);
    expect(queue.rows.some((row) => row.id === last.id)).toBe(true);
    expect(queue.selected).toBe(last.id);
  });

  it("leaves a filter alone when the job is already under it", async () => {
    // Somebody mid-way through narrowing the list did not ask for it to be reset, and a
    // notification about a job they can already see is not a reason to.
    feed({ event: "jobs", jobs: [view({ id: 5 as JobId, category: "Video" })] });
    queue.filter = { by: "category", category: "Video" };
    await reveal(5 as JobId);
    expect(queue.filter).toEqual({ by: "category", category: "Video" });
    expect(queue.selected).toBe(5);
  });

  it("says nothing about a job that is no longer in the list", async () => {
    // The notification outlives the row: `Clear completed` while a toast is on screen.
    feed({ event: "jobs", jobs: [view({ id: 6 as JobId })] });
    await reveal(99 as JobId);
    expect(queue.selected).toBeNull();
    expect(queue.expanded).toBeNull();
  });

  /**
   * `vortex-app --reveal 42` is a window started *by* the request, so the frontend reads
   * the id on mount and the list is still one round trip away. Dropping it there would
   * mean a click in the browser opens a window that looks like any other.
   */
  describe("before the list has arrived", () => {
    it("waits for the job and then opens it", async () => {
      await reveal(7 as JobId);
      expect(queue.selected, "there is nothing to select yet").toBeNull();

      feed({ event: "jobs", jobs: [view({ id: 7 as JobId, state: { kind: "completed" } })] });
      expect(queue.selected).toBe(7);
      expect(queue.expanded).toBe(7);
      expect(queue.visible.map((job) => job.id), "and it is in the list").toEqual([7]);
    });

    it("takes a job that arrives on its own, not only a whole list", async () => {
      await reveal(8 as JobId);
      feed({ event: "jobAdded", job: view({ id: 8 as JobId }) });
      expect(queue.selected).toBe(8);
    });

    it("forgets an id the full list does not have", async () => {
      // A list is the authoritative answer to "is there such a job". Holding the id past
      // that arms a reveal that fires on whatever is created next.
      await reveal(9 as JobId);
      feed({ event: "jobs", jobs: [view({ id: 1 as JobId })] });
      feed({ event: "jobAdded", job: view({ id: 9 as JobId }) });
      expect(queue.selected).toBeNull();
    });

    it("keeps only the last id asked for", async () => {
      await reveal(10 as JobId);
      await reveal(11 as JobId);
      feed({ event: "jobs", jobs: [view({ id: 10 as JobId }), view({ id: 11 as JobId })] });
      expect(queue.selected).toBe(11);
    });
  });
});

/**
 * Which changes the list is allowed to choreograph.
 *
 * The rule is "one thing happening", and the store is where it is decided because the row
 * cannot see the difference: a row appearing because a download started and a row appearing
 * because the filter widened are the same event as far as the row is concerned. Only the
 * thing that made the change knows which it was.
 */
describe("what counts as one thing happening", () => {
  const settle = async (): Promise<void> => {
    // Two, not one: `replaced` raises the flag *inside* a `tick` callback, so the second
    // await is what gets past its own resolution.
    await tick();
    await tick();
  };

  it("animates a download arriving", () => {
    feed({ event: "jobAdded", job: view() });
    expect(queue.animating).toBe(true);
  });

  it("animates a download leaving", () => {
    const job = view();
    feed({ event: "jobAdded", job }, { event: "jobRemoved", job: job.id });
    expect(queue.animating).toBe(true);
  });

  it("does not animate a filter change", async () => {
    queue.filter = { by: "category", category: "Video" };
    expect(queue.animating).toBe(false);
    await settle();
    expect(queue.animating).toBe(true);
  });

  it("does not animate a search", async () => {
    queue.search = "iso";
    expect(queue.animating).toBe(false);
    await settle();
    expect(queue.animating).toBe(true);
  });

  it("does not animate a page of rows the user scrolled to", async () => {
    feed({ event: "jobs", jobs: Array.from({ length: 60 }, () => view()) });
    await settle();
    queue.more();
    expect(queue.animating).toBe(false);
    await settle();
    expect(queue.animating).toBe(true);
  });

  it("does not animate the queue the daemon opens with", () => {
    // These were downloading before the window existed. Forty rows folding open would be
    // the app claiming forty downloads had just started.
    feed({ event: "jobs", jobs: [view(), view(), view()] });
    expect(queue.animating).toBe(false);
  });

  it("stays quiet when there is no next page to ask for", () => {
    feed({ event: "jobs", jobs: [view()] });
    queue.animating = true;
    queue.more();
    expect(queue.animating).toBe(true);
  });
});
