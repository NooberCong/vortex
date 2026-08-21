import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isPermissionGranted, requestPermission } from "@tauri-apps/plugin-notification";
import { bytes, duration, type Event, type JobId } from "@vortex/proto";

import { native } from "./ipc";
import { queue, reveal } from "./store.svelte";

/**
 * The two moments worth interrupting someone for.
 *
 * A download manager is something you start and then stop looking at, so the end of a
 * transfer is the one thing the app knows that the user cannot see. It says so once, per
 * job, and only for an ending the user did not ask for: a cancellation is already the
 * user's own doing and telling them about it is a notification that says "yes, you did
 * that".
 *
 * Nothing here is a progress toast. A notification per percentage point is how a download
 * manager teaches people to turn its notifications off.
 *
 * The toast is sent by `src-tauri/src/toast.rs` rather than by the notification plugin,
 * because the plugin's desktop path cannot report a click. A notification that says a
 * download finished and then does nothing when you click it is a worse promise than no
 * notification at all — so clicking one raises the window and opens that job's row.
 *
 * That last path outgrew notifications: the extension's popup can ask for a job too, and
 * it arrives by a different route to the same place. So [`watchReveals`] owns "something
 * outside this window named a job", whatever named it.
 */

/** Asked for once per session, and remembered even if the answer is no. */
let allowed: Promise<boolean> | null = null;

function permitted(): Promise<boolean> {
  allowed ??= isPermissionGranted()
    .then((granted) => (granted ? true : requestPermission().then((p) => p === "granted")))
    // A user who declined, or a desktop with no notification service at all, is not an
    // error condition. The download finished either way.
    .catch(() => false);
  return allowed;
}

/**
 * Announces a finished job.
 *
 * Sent whether or not the window has focus. A window that is open is not a window that is
 * being watched — it can be behind a browser, on another workspace, or simply not the
 * thing the user is looking at — and the app cannot tell the difference. The end of a
 * transfer is worth saying once either way.
 */
export async function announce(event: Event): Promise<void> {
  if (!native || event.event !== "jobFinished") return;
  const outcome = event.outcome;
  if (outcome.kind === "cancelled") return;

  const name = queue.get(event.job)?.view.filename ?? "Download";
  const body =
    outcome.kind === "completed"
      ? `Finished · ${bytes(outcome.bytes)} in ${duration(outcome.elapsedSecs)}`
      : `Failed · ${outcome.error}`;

  if (!(await permitted())) return;
  // Fire and forget: the click comes back through `watchReveals`, not through this call.
  await invoke("notify", { job: event.job, title: name, body });
}

/**
 * Wires everything outside the window that can name a job to the job it named.
 *
 * Two things arrive on this channel and they mean the same sentence — a click on a
 * finished transfer's toast, and a `Reveal` the extension sent through the daemon, which
 * reaches this process as `vortex-app --reveal <id>`. The native side has already raised
 * the window by the time either gets here; all that is left is the part only the list
 * knows how to do.
 *
 * The **cold** case cannot be an event. A `--reveal` that started this process is parsed
 * before the webview exists, so emitting it then would be shouting into an empty room; it
 * is read as state instead, exactly as `connected()` is and for the same reason. `reveal`
 * holds the id until the job turns up, because on a cold start the list has not arrived
 * yet either.
 */
export async function watchReveals(): Promise<() => void> {
  if (!native) return () => {};
  const stop = await listen<JobId>("vortex://reveal", (message) => {
    void reveal(message.payload);
  });
  // After the listener, never before: a warm second instance can emit while this is still
  // resolving, and an id read here that the listener then misses would be dropped twice.
  void invoke<JobId | null>("requested")
    .then((job) => {
      if (job !== null) void reveal(job);
    })
    .catch(() => {});
  return stop;
}
