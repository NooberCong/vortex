/**
 * The extension's small model of the daemon's queue.
 *
 * It holds one fact — **which jobs are unfinished** — because that is the number on the
 * toolbar badge and nothing else in the browser needs more. The popup asks for the full
 * list when it opens and throws it away when it closes; the daemon owns everything else,
 * as it owns everything else in Vortex.
 *
 * ## Why there is no subscription
 *
 * There nearly was. `Subscribe { Summary }` would keep this in step to the byte — and it
 * would deliver a frame per job at 2 Hz for as long as anything was downloading, into a
 * service worker whose entire cost model is that it sleeps after thirty idle seconds
 * (03 §Service worker lifetime). A background page kept permanently awake to animate a
 * number nobody is looking at is the wrong trade twice over.
 *
 * So the model is fed by the events the daemon **broadcasts to every client anyway** —
 * `JobAdded`, `JobStateChanged`, `JobFinished`, `JobRemoved`, which are structural and
 * arrive a handful of times per job rather than twenty times a second — and reconciled
 * against a full `List` whenever there is reason to think it has drifted: a fresh worker,
 * the liveness alarm, an opened popup.
 *
 * That also makes the badge *authoritative* rather than optimistic. It does not count a
 * submitted job; it counts a job the daemon has said it has. The difference shows up on
 * the path that matters — a handoff that reached `Submit` and then failed inside the
 * daemon leaves no phantom on the toolbar.
 *
 * Nothing durable lives in a module variable: the set is mirrored into `storage.session`
 * on every change, because the worker is evicted and the badge has to survive it.
 */

import { browser } from "wxt/browser";

import { isTerminal, type Event, type JobId, type JobState, type JobView } from "@vortex/proto";
import * as toolbar from "./toolbar";
import * as host from "./host";

const KEY = "activeJobs";

/**
 * How long to wait for the list.
 *
 * The daemon answers `List` out of its own memory, so a slow answer means it is not there.
 * Shorter than a probe's budget for the same reason `settings.ts` is: nothing downstream
 * of this is worth stalling a popup for.
 */
const LIST_TIMEOUT = 2000;

/**
 * The unfinished job ids, in memory, with `storage.session` as the durable mirror.
 *
 * Held as an object rather than read back per call so that every mutation is synchronous
 * once hydration is done — two events arriving in the same turn would otherwise
 * read-modify-write over each other. The same shape `settings.ts` uses, for the same
 * reason.
 */
let memo: Set<JobId> | null = null;
let hydrating: Promise<Set<JobId>> | null = null;

/**
 * What was last written and drawn, so that nothing happening costs nothing.
 *
 * The liveness alarm asks for the list every minute for as long as the browser is open. On
 * a machine with an idle queue that is otherwise a session-storage write and two toolbar
 * calls a minute, for ever, to say the same thing each time.
 */
let mirrored: string | null = null;

function active(): Promise<Set<JobId>> {
  if (memo) return Promise.resolve(memo);
  hydrating ??= browser.storage.session
    .get(KEY)
    .then((stored) => (memo ??= new Set((stored[KEY] as JobId[] | undefined) ?? [])))
    .catch(() => (memo ??= new Set<JobId>()));
  return hydrating;
}

/**
 * Folds one broadcast event into the count.
 *
 * Deliberately tolerant of events it was not the intended audience for. Everything
 * structural is broadcast to every connected client, so this sees jobs added by the app,
 * by the CLI and by another window — and it should: the badge is a statement about
 * Vortex, not about what this browser handed over.
 */
export async function adopt(event: Event): Promise<void> {
  const jobs = await active();
  switch (event.event) {
    case "jobAdded":
      track(jobs, event.job.id, event.job.state);
      break;
    case "jobStateChanged":
      track(jobs, event.job, event.state);
      break;
    // `JobFinished` and `JobRemoved` both mean "stop counting it", and a removal can
    // arrive for a job this set never saw — from another client, or from before the
    // worker woke. Deleting what is not there is free.
    case "jobFinished":
    case "jobRemoved":
      jobs.delete(event.job);
      break;
    default:
      return;
  }
  await settle(jobs);
}

/**
 * Asks the daemon for its whole queue, and trues the badge up against the answer.
 *
 * `null` means the daemon did not answer, which is **not** an empty queue — but it is a
 * reason to clear the badge. A number left over from the last time Vortex was running is a
 * claim that transfers are in progress inside a program that is not, and the popup's
 * daemon light is where "not running" gets said properly.
 */
export async function list(): Promise<JobView[] | null> {
  const answer = await host.request({ cmd: "list" }, (e) => e.event === "jobs", LIST_TIMEOUT);
  const jobs = answer?.event === "jobs" ? answer.jobs : null;

  const known = await active();
  known.clear();
  for (const job of jobs ?? []) track(known, job.id, job.state);
  await settle(known);
  return jobs;
}

function track(jobs: Set<JobId>, id: JobId, state: JobState): void {
  // Unfinished, not "moving". A paused or queued job is still one the user is expecting,
  // and a badge that dropped to nothing the moment everything was paused would be saying
  // the queue is empty when it is merely still.
  if (isTerminal(state)) jobs.delete(id);
  else jobs.add(id);
}

async function settle(jobs: Set<JobId>): Promise<void> {
  const ids = [...jobs];
  // Membership, not size: two lists a minute apart can hold the same number of different
  // jobs, and the mirror has to stay accurate even when the badge does not change.
  const now = ids.join(",");
  if (now === mirrored) return;
  mirrored = now;

  await browser.storage.session.set({ [KEY]: ids });
  await toolbar.paint(ids.length);
}

/** Test seam. */
export function __resetForTests(): void {
  memo = null;
  hydrating = null;
  mirrored = null;
}
