import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { bytes, duration, type Event } from "@vortex/proto";

import { native } from "./ipc";
import { queue } from "./store.svelte";

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
 * Announces a finished job, unless the window is already showing it.
 *
 * Suppressed while the window has focus: the row is right there, animating to its finished
 * state, and a toast over the top of it says nothing the user is not already looking at.
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

  if (await getCurrentWindow().isFocused()) return;
  if (!(await permitted())) return;
  sendNotification({ title: name, body });
}
