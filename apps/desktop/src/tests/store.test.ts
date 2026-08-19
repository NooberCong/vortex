import { beforeEach, describe, expect, it } from "vitest";

import type { Event, JobId, JobState, JobView, SummaryFrame } from "@vortex/proto";

import { isActive, isDone, needsAttention, queue } from "$lib/store.svelte";

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

const feed = (...events: Event[]) => events.forEach((e) => queue.apply(e));

beforeEach(() => {
  next = 1;
  feed({ event: "jobs", jobs: [] });
  queue.filter = { by: "state", state: "active" };
  queue.search = "";
  queue.selected = null;
  queue.expanded = null;
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

describe("the sidebar's arithmetic", () => {
  const states: JobState[] = [
    { kind: "downloading" },
    { kind: "paused" },
    { kind: "queued" },
    { kind: "completed" },
    { kind: "failed", error: "x" },
  ];

  it("puts every job in exactly one of active, queued and done", () => {
    // The three counts are the whole navigation. If they do not add up, the user has a
    // download they cannot find.
    feed({ event: "jobs", jobs: states.map((state) => view({ state })) });
    const { active, queued, done } = queue.counts;
    expect(active + queued + done).toBe(states.length);
    expect({ active, queued, done }).toEqual({ active: 2, queued: 1, done: 2 });
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

  it("filters by state, then by search", () => {
    expect(queue.visible.map((j) => j.id)).toEqual([2, 1]);
    queue.search = "ubuntu";
    expect(queue.visible.map((j) => j.id)).toEqual([1]);
  });

  it("searches the host as well as the filename", () => {
    queue.search = "cdn.edu";
    expect(queue.visible.map((j) => j.id)).toEqual([2]);
  });

  it("treats an empty search as no filtering, not as matching nothing", () => {
    queue.search = "   ";
    expect(queue.visible).toHaveLength(2);
  });

  it("filters by category across every state", () => {
    queue.filter = { by: "category", category: "Video" };
    expect(queue.visible.map((j) => j.id)).toEqual([3, 2]);
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
});
