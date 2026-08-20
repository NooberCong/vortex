import {
  ProgressBarStatus,
  type ProgressBarState,
  type Window as TauriWindow,
} from "@tauri-apps/api/window";
import { rate } from "@vortex/proto";

import { daemon } from "./ipc";
import { queue } from "./store.svelte";

/**
 * The one number that is true of the whole app (05 §Layout).
 *
 * Sampled at 2 Hz — the same cadence the daemon sends summaries at, so the trace has one
 * bar per frame it was actually told about rather than a smooth curve it invented. Three
 * things read it: the titlebar readout, the tray tooltip and the taskbar progress bar, and
 * they all read the same sample, which is why the sampling is here and not in any of them.
 */

const HZ = 500;
/** Twenty samples is ten seconds — long enough to show a trend, short enough to be now. */
const HISTORY = 20;

class Aggregate {
  /** Oldest first. */
  history = $state.raw<number[]>([]);
}

export const aggregate = new Aggregate();

/** Only the one method is needed, and narrowing says so. */
type Taskbar = Pick<TauriWindow, "setProgressBar">;

/**
 * What the taskbar button should be showing right now.
 *
 * Three states, in the order they answer the user's question. Nothing working is a plain
 * button — the app is idle and should look idle. Something working and measurable is the
 * filled bar, which is the one readout a download manager owes the taskbar. Something
 * working that *cannot* be measured — a chunked stream, an extractor that will not say how
 * long the file is, a mux — is the indeterminate sweep: on Windows it animates in the same
 * slot the filled bar would use, so "still going" is glanceable without a number the app
 * would have to invent to show it.
 *
 * The last case is the point of this function. Sending `None` there, which is what a
 * measurable-or-nothing rule does, makes a working app look like an idle one.
 */
export function taskbar(working: number, progress: number | null): ProgressBarState {
  if (working === 0) return { status: ProgressBarStatus.None };
  if (progress === null) return { status: ProgressBarStatus.Indeterminate };
  return { status: ProgressBarStatus.Normal, progress: Math.round(progress * 100) };
}

/**
 * Starts sampling. Returns the stop function; the app calls it on teardown.
 *
 * The two platform surfaces are updated from the same tick and are allowed to fail
 * silently: a taskbar that does not show progress is a cosmetic loss, and a window that
 * refuses to open because of one would not be.
 */
export function sample(window: Taskbar | null): () => void {
  /**
   * The last thing the taskbar was told, so that it is only told again when the answer
   * changes. Twice a second is the right cadence for a *number*; re-asserting the
   * indeterminate sweep at that cadence is asking the shell to restart an animation it is
   * already running.
   */
  let sent: string | null = null;

  const timer = setInterval(() => {
    const bps = queue.throughput;
    aggregate.history = [...aggregate.history, bps].slice(-HISTORY);

    void daemon
      .tooltip(queue.running > 0 ? `Vortex — ${rate(bps)}` : "Vortex")
      .catch(() => {});

    const next = taskbar(queue.working, queue.progress);
    const key = JSON.stringify(next);
    if (key === sent) return;
    sent = key;
    void window?.setProgressBar(next).catch(() => {});
  }, HZ);

  return () => clearInterval(timer);
}
