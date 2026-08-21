import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event, JobId, JobState, JobView } from "@vortex/proto";
import * as host from "@/src/host";
import * as queue from "@/src/queue";

/**
 * The toolbar badge, and the model behind it.
 *
 * The badge is the floor of the whole acknowledgement (03 §2): the in-page receipt needs a
 * page with a content script in it and the popup needs somebody to open it, and a takeover
 * started from a PDF viewer has neither. So the properties worth pinning are the ones that
 * decide whether a user can trust the number:
 *
 * - it counts what the **daemon** says it has, never what this extension submitted;
 * - it counts everything **unfinished**, not everything moving, because a paused download
 *   is still a download somebody is expecting;
 * - and it says **nothing at all** rather than something stale when the daemon is gone.
 */

type Badge = {
  setBadgeText: (details: { text: string }) => unknown;
  setTitle: (details: { title: string }) => unknown;
  setBadgeBackgroundColor: (details: { color: string }) => unknown;
};

const action = () => (browser as unknown as { action: Badge }).action;

function drawn(): { text: string; title: string } {
  const text = vi.mocked(action().setBadgeText).mock.calls.at(-1)?.[0];
  const title = vi.mocked(action().setTitle).mock.calls.at(-1)?.[0];
  return { text: text?.text ?? "", title: title?.title ?? "" };
}

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

/** A stand-in for `vortex-host`: records commands, and answers the ones we tell it to. */
function fakeDaemon(reply: (command: Command) => Event | null) {
  const sent: Command[] = [];
  const listeners: Array<(event: unknown) => void> = [];
  const port = {
    postMessage(command: Command) {
      sent.push(command);
      const event = reply(command);
      if (event) queueMicrotask(() => listeners.forEach((l) => l(event)));
    },
    disconnect() {},
    onMessage: { addListener: (l: (event: unknown) => void) => listeners.push(l) },
    onDisconnect: { addListener: () => {} },
  };
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue(port as never);
  return sent;
}

/** The daemon answering `List` with exactly these jobs. */
const answering = (jobs: JobView[]) =>
  fakeDaemon((command) => (command.cmd === "list" ? { event: "jobs", jobs } : null));

beforeEach(() => {
  fakeBrowser.reset();
  host.__resetForTests();
  queue.__resetForTests();
  next = 1;
  vi.restoreAllMocks();
  vi.spyOn(action(), "setBadgeText");
  vi.spyOn(action(), "setTitle");
  vi.spyOn(action(), "setBadgeBackgroundColor");
});

describe("what the badge counts", () => {
  it("shows a number the moment the daemon says it has the job", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    expect(drawn()).toEqual({ text: "1", title: "Vortex — 1 transfer" });

    await queue.adopt({ event: "jobAdded", job: view({ id: 2 as JobId }) });
    expect(drawn()).toEqual({ text: "2", title: "Vortex — 2 transfers" });
  });

  it("clears the badge entirely at zero rather than drawing one", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    await queue.adopt({
      event: "jobFinished",
      job: 1 as JobId,
      outcome: { kind: "completed", path: "D:\\x", bytes: 1000, elapsedSecs: 3, verified: null },
    });
    expect(drawn()).toEqual({ text: "", title: "Vortex" });
  });

  it("keeps counting a job that has stopped moving but has not finished", async () => {
    // A badge that emptied when everything was paused would be saying the queue is empty
    // when it is merely still — and the user would go looking for a download nothing
    // claims to be holding.
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    for (const state of [
      { kind: "paused" },
      { kind: "queued" },
      { kind: "stalled", reason: "The server hung up." },
      { kind: "needsDecision", decision: { kind: "diskFull", detail: "No room." } },
    ] as JobState[]) {
      await queue.adopt({ event: "jobStateChanged", job: 1 as JobId, state });
      expect(drawn().text, `${state.kind} stopped being counted`).toBe("1");
    }
  });

  it("stops counting a job that reached a terminal state", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    await queue.adopt({ event: "jobAdded", job: view({ id: 2 as JobId }) });
    await queue.adopt({ event: "jobStateChanged", job: 1 as JobId, state: { kind: "completed" } });
    expect(drawn().text).toBe("1");
    await queue.adopt({
      event: "jobStateChanged",
      job: 2 as JobId,
      state: { kind: "failed", error: "404" },
    });
    expect(drawn().text).toBe("");
  });

  it("forgets a job that was removed, whoever removed it", async () => {
    // Structural events are broadcast to every client, so this arrives from the app and
    // from the CLI as readily as from this browser. That is correct: the badge is a
    // statement about Vortex, not about what this browser handed over.
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    await queue.adopt({ event: "jobRemoved", job: 1 as JobId });
    expect(drawn().text).toBe("");
  });

  it("ignores the events that are somebody else's business", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    const before = vi.mocked(action().setBadgeText).mock.calls.length;
    await queue.adopt({ event: "pong" });
    await queue.adopt({
      event: "urlExpired",
      job: 1 as JobId,
      hint: { url: "https://example.com/x", reason: "403" },
    });
    expect(vi.mocked(action().setBadgeText).mock.calls.length).toBe(before);
  });

  it("is achromatic, because a download starting is not something being wrong", async () => {
    // `--attention` is the only colour the tokens allow outside the segment map and it
    // means something has gone wrong (05 §The one rule). The signal here is that a badge
    // is on an icon that normally carries none, not that it is loud.
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    const colour = vi.mocked(action().setBadgeBackgroundColor).mock.calls.at(-1)?.[0].color;
    expect(colour).toBe("#5b6577");
  });

  it("stops being precise above a number nobody reads precisely", async () => {
    for (let id = 1; id <= 100; id++) {
      await queue.adopt({ event: "jobAdded", job: view({ id: id as JobId }) });
    }
    expect(drawn()).toEqual({ text: "99+", title: "Vortex — 100 transfers" });
  });
});

describe("asking the daemon", () => {
  it("replaces the count wholesale, because a list is not a delta", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    answering([view({ id: 5 as JobId }), view({ id: 6 as JobId })]);

    const jobs = await queue.list();
    expect(jobs?.map((job) => job.id)).toEqual([5, 6]);
    expect(drawn().text, "job 1 finished while the worker was asleep").toBe("2");
  });

  it("counts only the unfinished ones out of the list", async () => {
    answering([
      view({ id: 1 as JobId }),
      view({ id: 2 as JobId, state: { kind: "completed" } }),
      view({ id: 3 as JobId, state: { kind: "paused" } }),
    ]);
    await queue.list();
    expect(drawn().text).toBe("2");
  });

  it("costs nothing when nothing changed", async () => {
    // The liveness alarm asks every minute for as long as the browser is open. An idle
    // queue must not mean a session-storage write and two toolbar calls a minute, for ever.
    answering([view({ id: 1 as JobId })]);
    await queue.list();
    const drawnOnce = vi.mocked(action().setBadgeText).mock.calls.length;

    await queue.list();
    await queue.list();
    expect(vi.mocked(action().setBadgeText).mock.calls.length).toBe(drawnOnce);
  });

  it("notices a list of the same size holding different jobs", async () => {
    // One finished and one started between two ticks. The count is unchanged and the badge
    // has nothing to redraw, but the set behind it is now wrong unless it is written.
    let listing = [view({ id: 1 as JobId })];
    fakeDaemon((command) => (command.cmd === "list" ? { event: "jobs", jobs: listing } : null));

    await queue.list();
    listing = [view({ id: 2 as JobId })];
    await queue.list();

    // A fresh worker, rehydrating from session storage: job 1 has to be gone from it and
    // job 2 has to be in it, or the next event is folded into a set that was never updated.
    queue.__resetForTests();
    await queue.adopt({ event: "jobRemoved", job: 2 as JobId });
    expect(drawn().text).toBe("");
  });

  it("says nothing at all when the daemon does not answer", async () => {
    // A number left over from the last time Vortex was running is a claim that transfers
    // are in progress inside a program that is not. The popup's daemon light is where
    // "not running" gets said properly.
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    expect(drawn().text).toBe("1");

    fakeDaemon(() => null);
    vi.useFakeTimers();
    const asked = queue.list();
    await vi.advanceTimersByTimeAsync(2000);
    vi.useRealTimers();

    await expect(asked).resolves.toBeNull();
    expect(drawn().text).toBe("");
  });
});

describe("surviving an eviction", () => {
  it("picks the count back up from session storage", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    await queue.adopt({ event: "jobAdded", job: view({ id: 2 as JobId }) });

    // The MV3 worker is torn down and a new one starts with an empty module scope. What
    // it must not do is start counting from zero and paint a "1" over the user's two.
    queue.__resetForTests();

    await queue.adopt({ event: "jobAdded", job: view({ id: 3 as JobId }) });
    expect(drawn().text).toBe("3");
  });

  it("does not double-count a job it already knew about", async () => {
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    queue.__resetForTests();
    await queue.adopt({ event: "jobAdded", job: view({ id: 1 as JobId }) });
    expect(drawn().text).toBe("1");
  });
});
