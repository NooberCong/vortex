import { beforeEach, describe, expect, it } from "vitest";

import type { Event, JobId, JobState, JobView } from "@vortex/proto";

import { requestRemove } from "$lib/actions";
import { queue } from "$lib/store.svelte";

/**
 * Which removals stop to ask, and which do not.
 *
 * The dialog exists for one case: a finished download, where the row and the file it
 * produced are two separate things and only the user knows which of them to keep. Every
 * other state has no finished file at stake — the engine writes into `.vxpart` and renames
 * at the very end, and the daemon deletes the partial work either way — so a question there
 * would be a question whose answer changes nothing. That is worth pinning, because the
 * failure is silent in both directions: asking too often trains people to click through,
 * and asking too rarely loses a file.
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
    state: { kind: "completed" },
    priority: "Normal",
    total: 1000,
    completed: 1000,
    mode: "Parallel",
    protocol: "H2",
    addresses: 1,
    createdAt: 1_700_000_000 + Number(id),
    finishedAt: 1_700_000_100 + Number(id),
    verified: null,
    retries: { total: 0, recovered: 0 },
    media: null,
    ...over,
  };
}

const feed = (...events: Event[]) => events.forEach((e) => queue.apply(e));

/** Puts one job in the list in the given state and asks to remove it. */
function ask(state: JobState): JobId | null {
  const one = view({ state });
  feed({ event: "jobs", jobs: [one] });
  requestRemove(queue.get(one.id)!);
  return queue.removing;
}

beforeEach(() => {
  next = 1;
  feed({ event: "jobs", jobs: [] });
  queue.removing = null;
});

describe("asking before removing", () => {
  it("asks about a finished download, which is the only one with a file to lose", () => {
    expect(ask({ kind: "completed" })).toBe(1);
  });

  it.each<JobState>([
    { kind: "queued" },
    { kind: "probing" },
    { kind: "downloading" },
    { kind: "paused" },
    { kind: "muxing" },
    { kind: "stalled", reason: "no route to host" },
    { kind: "failed", error: "the server closed the connection" },
  ])("removes a $kind job outright, because nothing was ever renamed into place", (state) => {
    expect(ask(state)).toBeNull();
  });
});

describe("the sheet's grip on its job", () => {
  it("lets go when the job it is asking about is removed from under it", () => {
    // The CLI, or a second window. A question about a row that is gone has nothing behind
    // it, and the sheet must not sit there waiting for an answer that cannot apply.
    const one = view();
    feed({ event: "jobs", jobs: [one] });
    requestRemove(queue.get(one.id)!);
    expect(queue.removing).toBe(one.id);

    feed({ event: "jobRemoved", job: one.id });
    expect(queue.removing).toBeNull();
  });

  it("keeps asking when some other job is removed", () => {
    const [one, two] = [view(), view()];
    feed({ event: "jobs", jobs: [one, two] });
    requestRemove(queue.get(two.id)!);

    feed({ event: "jobRemoved", job: one.id });
    expect(queue.removing).toBe(two.id);
  });
});
