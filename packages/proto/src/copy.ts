/**
 * The words for the states (05 §Copy).
 *
 * `JobState`'s own definition in `crates/vortex-proto/src/lib.rs` calls itself "the
 * vocabulary the UI signposts with… user-visible states, not internal jargon" — which is a
 * promise that only holds if every surface says the same word. Two of them now do: the
 * desktop list, and the extension popup's transfer rows. A queue that reads *Checking* in
 * the window and *Probing* in the browser has taught the user that they are two products.
 *
 * It lives here rather than in either app for the same reason `format.ts` does. The
 * difference is that the numbers have a Rust half to agree with and these do not: nothing
 * in the daemon renders a state, so there is one implementation and this is it.
 */

import type { JobState } from "./bindings/JobState";

/**
 * The word for a state, or `null` where the surface should say something better.
 *
 * `downloading` is deliberately `null`. In the desktop list the segment map is already
 * moving and already the only coloured thing on the row, so a label saying so would be the
 * second time the row said one thing (05 §The one rule); in the popup, where there is no
 * map, the byte count takes the slot. Both are more informative than the word, which is
 * why the word is not offered.
 */
export function label(state: JobState): string | null {
  switch (state.kind) {
    case "queued":
      return "Queued";
    case "probing":
      return "Checking";
    case "downloading":
      return null;
    case "stalled":
      return "Retrying";
    case "muxing":
      return "Combining";
    case "paused":
      return "Paused";
    case "needsDecision":
      return "Needs you";
    case "completed":
      return "Downloaded";
    case "failed":
      return "Failed";
  }
}

/**
 * Has this job stopped for good?
 *
 * The TypeScript half of `JobState::is_terminal`. Everything else is unfinished — queued,
 * paused, waiting for an answer — and unfinished is what the extension's toolbar badge
 * counts, because a paused download is still a download the user is expecting.
 */
export function isTerminal(state: JobState): boolean {
  return state.kind === "completed" || state.kind === "failed";
}
