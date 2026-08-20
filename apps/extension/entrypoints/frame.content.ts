import { MEDIA_CAPTURE } from "@/src/build";
import { excludeMatches } from "@/src/denylist";
import { post } from "@/src/messages";
import { EVERY, Orphans } from "@/src/orphan";

/**
 * Channel 5's other half — the frame reporter (03 §5, embedded players).
 *
 * `entrypoints/content.ts` is `allFrames: false`, for two good reasons that both remain
 * true: the overlay belongs to the page rather than to each of its twenty tracking frames,
 * and renewal works from the top document whichever frame owns the player. The orphan
 * watcher was swept along with them, and it should not have been. It is the one part of
 * that script whose entire job is to notice a `<video>`, and an enormous share of the web's
 * players sit in a cross-origin `<iframe>` — every `/embed/` URL, every third-party player
 * host. From the top document those are not merely hard to find, they are unreachable:
 * `contentDocument` is `null` across origins, so no amount of `querySelectorAll` up there
 * will ever see them, and the channel written specifically to catch "all four network
 * channels missed" could never fire on any of them.
 *
 * So the smallest possible script goes into every frame, and it reports one bit: *there is
 * a real player in here*. It draws nothing, it answers no messages, and it never touches
 * the frame's DOM beyond reading it. Everything that decides what to do with the signal —
 * the capture switch, the denylist, the per-origin opt-out, whether a ladder is already
 * known — stays in the background where the other four channels are already governed by it.
 *
 * The watcher is the same `Orphans` the top document runs, deliberately: a second
 * definition of "a real player has been sitting here unexplained" is how the badge and the
 * extractor end up disagreeing about what is on the page. Two consequences of reusing it
 * as-is, both accepted rather than overlooked:
 *
 * - Nothing calls `attributed()` in here, because ladders are delivered to the top
 *   document. So a frame whose stream *was* attributed still asks once. The background
 *   already drops that: `probePage` returns early when the tab has candidates, which is
 *   the same check that makes the top document's own late asks free.
 * - `Orphans` declines to ask from a document the daemon could not fetch, judged by the
 *   frame's own protocol. A player in an `about:srcdoc` or `blob:` frame is therefore not
 *   reported. Rare next to ordinary `https` embeds, and the alternative — teaching the
 *   watcher that its document and the page being probed can be different things — buys
 *   that case at the cost of the property that makes the class worth sharing.
 */
export default defineContentScript({
  matches: ["<all_urls>"],
  excludeMatches: excludeMatches(),
  runAt: "document_idle",
  // The one script here that wants every frame. It is also the only one cheap enough to
  // justify it: a `querySelectorAll("video")` a second against a document that has none.
  allFrames: true,

  main(ctx) {
    // Folds to a literal, so the store build contains none of what follows even if the
    // entrypoint itself is ever left in (`wxt.config.ts` drops it, and
    // `tests/store-build.test.ts` holds both lines).
    if (!MEDIA_CAPTURE) return;
    // The top document has the real agent, with its own watcher, the overlay and renewal.
    // Running a second watcher there would ask the same question twice.
    if (window.top === window) return;

    // The ask carries no URL. The background names the page from `sender.tab.url`, which
    // the browser fills in — a subframe is routinely someone else's code, and it has no
    // business choosing what the daemon's extractor goes and fetches.
    const orphans = new Orphans(() => {
      void post({ kind: "framePlayer" });
    });
    // `ctx`'s schedule, not a bare `setInterval`. After an extension reload every
    // `browser.runtime` call from this frame throws synchronously, and a timer that
    // outlives the runtime is one uncaught "Extension context invalidated" per second for
    // as long as the tab stays open.
    ctx.setInterval(() => orphans.look(), EVERY);
  },
});
