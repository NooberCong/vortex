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
  // Fire and forget: the click comes back through `watchClicks`, not through this call.
  await invoke("notify", { job: event.job, title: name, body });
}

/**
 * Wires the click on a notification to the job it was about.
 *
 * The native side has already raised the window by the time this runs — that part cannot
 * wait for a webview — so all that is left is the part only the list knows how to do.
 */
export async function watchClicks(): Promise<() => void> {
  if (!native) return () => {};
  return listen<JobId>("vortex://notification", (message) => {
    void reveal(message.payload);
  });
}
