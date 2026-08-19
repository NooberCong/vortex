import { ProgressBarStatus, type Window as TauriWindow } from "@tauri-apps/api/window";
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
 * Starts sampling. Returns the stop function; the app calls it on teardown.
 *
 * The two platform surfaces are updated from the same tick and are allowed to fail
 * silently: a taskbar that does not show progress is a cosmetic loss, and a window that
 * refuses to open because of one would not be.
 */
export function sample(window: Taskbar | null): () => void {
  const timer = setInterval(() => {
    const bps = queue.throughput;
    aggregate.history = [...aggregate.history, bps].slice(-HISTORY);

    void daemon
      .tooltip(queue.running > 0 ? `Vortex — ${rate(bps)}` : "Vortex")
      .catch(() => {});

    const progress = queue.progress;
    void window
      ?.setProgressBar(
        progress === null
          ? { status: ProgressBarStatus.None }
          : { status: ProgressBarStatus.Normal, progress: Math.round(progress * 100) },
      )
      .catch(() => {});
  }, HZ);

  return () => clearInterval(timer);
}
