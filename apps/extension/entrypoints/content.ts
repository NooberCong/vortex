import { browser } from "wxt/browser";

import type { MediaCandidate } from "@vortex/proto";
import { MEDIA_CAPTURE } from "@/src/build";
import { excludeMatches } from "@/src/denylist";
import type { FromPage, ToPage } from "@/src/messages";
import { EVERY, Orphans } from "@/src/orphan";
import { Overlay } from "@/src/overlay";

/**
 * The page agent.
 *
 * It does two things the background cannot, both of which need to be inside the page:
 * it draws the overlay, and it re-acquires an expired URL with the page's own credentials.
 *
 * It is inert until spoken to. On a page with no video it registers one message listener,
 * sends one message, and stops — which is the whole cost of having it everywhere.
 *
 * The DRM denylist is in `exclude_matches` as well as in the background's checks. On
 * those origins this script is never loaded at all, which is a stronger statement than
 * "it decided to do nothing" and one that survives someone reading the built artifact.
 *
 * **Everything with a heartbeat is owned by `ctx`.** An extension reload or update leaves
 * this script running in a page whose runtime is gone, and from that moment every
 * `browser.runtime` call throws *synchronously* — which no `.catch()` on a returned promise
 * can help with. A bare `setInterval` in here is therefore not a timer, it is an
 * "Extension context invalidated" every second for as long as the tab stays open.
 */
export default defineContentScript({
  matches: ["<all_urls>"],
  excludeMatches: excludeMatches(),
  runAt: "document_idle",
  // Top document only. The overlay belongs to the page, not to each of the twenty
  // tracking frames on it, and renewal works from here regardless of which frame owns
  // the player: cookies are sent for the *target* origin, so the top document's `fetch`
  // carries the same credentials an embedded player's would.
  allFrames: false,

  main(ctx) {
    let overlay: Overlay | null = null;
    // Channel 5, constructed only where there is one — the store build has no streaming
    // code in it at all, and that is a property of the artifact rather than of its
    // behaviour (03 §Store strategy).
    let orphans: Orphans | null = null;

    const ensure = (): Overlay => {
      overlay ??= new Overlay({
        download(selection, pageTitle) {
          void post({ kind: "download", selection, pageTitle });
        },
        dismiss(origin) {
          void post({ kind: "dismiss", origin });
          overlay?.destroy();
          overlay = null;
        },
      });
      return overlay;
    };

    browser.runtime.onMessage.addListener((message, _sender, respond) => {
      const incoming = message as ToPage;

      if (incoming.kind === "candidates") {
        if (MEDIA_CAPTURE) {
          if (incoming.candidates.length > 0) orphans?.attributed();
          ensure().show(incoming.candidates);
        }
        return false;
      }

      if (incoming.kind === "renew") {
        void reacquire(incoming.url).then(respond);
        return true;
      }

      return false;
    });

    if (MEDIA_CAPTURE) {
      // Built before anything can answer, so a ladder that arrives on the very first turn
      // of the microtask queue still finds something to tell. The schedule is `ctx`'s: it
      // stops calling on invalidation, where a plain interval would keep running against a
      // dead runtime and throw once a second for as long as the tab is open.
      orphans = new Orphans((pageUrl) => {
        void post({ kind: "orphan", pageUrl });
      });
      ctx.setInterval(() => orphans?.look(), EVERY);

      // A worker restart, a back-forward navigation or a single-page route change all
      // leave the background holding a ladder this script has never seen. Asking is one
      // message.
      void post({ kind: "ready" }).then((candidates) => {
        const found = candidates as MediaCandidate[] | undefined;
        if (found?.length) {
          orphans?.attributed();
          ensure().show(found);
        }
      });
    }

    // A badge left on the page after the extension went away is a button that cannot do
    // anything, which is worse than no button.
    ctx.onInvalidated(() => {
      overlay?.destroy();
      overlay = null;
    });
  },
});

/**
 * One message to the background, and never an exception either way.
 *
 * The `try` is not belt-and-braces. Once the extension has been reloaded under an open
 * page, `sendMessage` throws before it ever returns a promise, so the `.catch` is not
 * attached to anything — which is the difference between a silent no-op and an uncaught
 * "Extension context invalidated" in the console of every tab the user has open.
 */
function post(message: FromPage): Promise<unknown> {
  try {
    return browser.runtime.sendMessage(message).catch(() => undefined);
  } catch {
    return Promise.resolve(undefined);
  }
}

/**
 * Re-acquires an expired URL in page context (03 §Handoff 3).
 *
 * `Range: bytes=0-0` because this is a probe, not a download — one byte answers both
 * "does it still work" and "where did it redirect to". `credentials: "include"` because
 * the whole point is to be the page rather than another process. Whatever the browser
 * ends up at is the fresh URL, and the request is observed by the background's own
 * `webRequest` listeners on the way, so the ledger gains a full envelope for it.
 */
async function reacquire(url: string): Promise<string | null> {
  try {
    const response = await fetch(url, {
      method: "GET",
      headers: { Range: "bytes=0-0" },
      credentials: "include",
      redirect: "follow",
      cache: "no-store",
    });
    // A 403 on the renewed URL means the page cannot mint one either — which is the
    // signal to stop, not to try again.
    if (!response.ok && response.status !== 206) return null;
    // Read nothing. One byte is already more than we need, and leaving the body
    // undrained lets the browser cancel it.
    void response.body?.cancel();
    return response.url || url;
  } catch {
    return null;
  }
}
