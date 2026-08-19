/**
 * One animation frame loop for the whole window.
 *
 * Forty rows each running their own `requestAnimationFrame` is forty callbacks the browser
 * has to schedule and forty closures it has to keep alive, and it is the difference between
 * a list that holds 60 fps and one that does not (05 §Quality floor). Everything that
 * animates — the shimmer on in-flight regions, the shatter, the throughput trace —
 * subscribes here instead.
 *
 * The loop does not run when nobody is subscribed, which is most of the time: a window with
 * nothing downloading has no shimmer to draw and drops to zero frames rather than to
 * sixty idle ones. That is the difference between the 0.5% idle CPU floor and missing it.
 */

type Frame = (now: number) => void;

const subscribers = new Set<Frame>();
let handle: number | null = null;

function pump(now: number): void {
  handle = null;
  // Copied because a subscriber may unsubscribe itself — a map whose row just scrolled out
  // of view does exactly that.
  for (const frame of [...subscribers]) frame(now);
  if (subscribers.size > 0) handle = requestAnimationFrame(pump);
}

/** Subscribes until the returned function is called. */
export function onFrame(frame: Frame): () => void {
  subscribers.add(frame);
  if (handle === null) handle = requestAnimationFrame(pump);
  return () => {
    subscribers.delete(frame);
    if (subscribers.size === 0 && handle !== null) {
      cancelAnimationFrame(handle);
      handle = null;
    }
  };
}

/** Test seam. Nothing in the app has a reason to ask. */
export function running(): boolean {
  return handle !== null;
}
