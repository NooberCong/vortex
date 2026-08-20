/**
 * Channel 5 — the page itself (03 §5).
 *
 * The four channels in front of this one all watch the *network*, and they all assume the
 * same thing: that a page which plays video fetches something recognisable as video. Most
 * do. A few of the largest sites do not — YouTube ships no `.m3u8` and no `.mpd`, describes
 * its streams in JSON embedded in the document, and pulls the media itself from
 * `videoplayback?…`, a URL with no extension, in ranges too small to look like a file. Every
 * pattern the sniffer matches on misses, so nothing is ever probed and no badge ever
 * appears — and nothing anywhere says why.
 *
 * This is the answer to that, and it is deliberately the *last* one: when a real player has
 * been sitting on the page long enough that any of the other channels would have produced a
 * ladder by now, hand the daemon the **page URL** and let the extractor look at it.
 *
 * The cost of being wrong is a subprocess that finds nothing, so the guards are about not
 * being wrong very often rather than about safety:
 *
 * - **A real player**, by the same rules that decide where a badge goes — big enough,
 *   painted, not a sound effect, not a decorative background loop.
 * - **With something loaded.** Metadata and a duration, so a `<video>` waiting for a source
 *   it will never get does not ask. Not *playing*: someone who opens a page and reads the
 *   description before pressing play still wants the button.
 * - **After a settling period**, because the ordinary channels are faster than this one and
 *   asking first would run an extractor over every HLS site on the web.
 * - **Once per page.** Re-armed on a route change, because on a single-page app the content
 *   script is never reloaded and the second video is a different video.
 *
 * It does not own its own schedule. `look()` is called by the content script's own
 * context, which stops calling it when the extension is reloaded out from under the page —
 * a bare `setInterval` would keep running against a dead runtime, and every call into it
 * throws *synchronously*, which no `.catch()` on the sending side can help with.
 */

import { playersOnPage, usablePlayers, type Player } from "./overlay/anchor";

/**
 * How long a loaded player waits before the page is offered to the extractor.
 *
 * Long enough that a manifest the sniffer *can* see has been probed, parsed and delivered
 * — that round trip is a second or two — and short enough that nobody reads it as broken.
 */
export const SETTLE = 3000;

/** How often the caller should look. Cheap: one `querySelectorAll` and a style read each. */
export const EVERY = 1000;

export class Orphans {
  /** The page this watcher is currently armed for. */
  private page = "";
  /** Set once this page has either asked or been answered. */
  private settled = true;
  /** When a usable player was first seen loaded on this page. */
  private since: number | null = null;

  constructor(private readonly ask: (pageUrl: string) => void) {}

  /**
   * A ladder arrived for this page from any channel, so there is nothing to ask about.
   *
   * Deliberately not reset by anything but a route change: a page whose stream *was*
   * attributed does not become worth an extraction later because a second player appeared.
   */
  attributed(): void {
    // The page is recorded along with the answer. A ladder can arrive before this has ever
    // looked — the background answers `ready` asynchronously — and without the page there
    // is nothing to distinguish "already answered" from "never armed", so the first look
    // would arm and start the clock on a page that has its badge already.
    this.page = location.href;
    this.settled = true;
  }

  /** One pass. Call it on a timer the caller owns; `EVERY` is the interval it expects. */
  look(): void {
    // On a single-page app the document is never reloaded, so this is the only notice
    // there is that the video the user is looking at is a different video.
    if (location.href !== this.page) this.arm();
    if (this.settled) return;

    if (!hasLoadedPlayer()) {
      // Gone again — a player that was swapped out mid-settle starts its clock over.
      this.since = null;
      return;
    }
    this.since ??= Date.now();
    if (Date.now() - this.since < SETTLE) return;

    this.settled = true;
    this.ask(this.page);
  }

  private arm(): void {
    this.page = location.href;
    this.since = null;
    // Only a page the daemon could fetch. A `file://` document or an extension page has
    // nothing for an extractor to look at and no business being sent anywhere.
    this.settled = !/^https?:$/i.test(location.protocol);
  }
}

/**
 * Is there a real, loaded player in this document right now?
 *
 * Exported because two callers ask the same question for the same reason, and a second
 * definition of "a real player" is the kind of drift that ends with the badge and the
 * extractor disagreeing about what is on the page. The watcher above asks it about the
 * document it lives in; `entrypoints/frame.content.ts` asks it inside a subframe, where the top
 * document is not merely unlikely to find the answer but structurally unable to — a
 * cross-origin `contentDocument` is not readable, so no `querySelectorAll` from up there
 * will ever see the player.
 */
export function hasLoadedPlayer(): boolean {
  return usablePlayers(playersOnPage()).some(loaded);
}

/**
 * Has this player got something in it?
 *
 * `readyState >= HAVE_METADATA` and a finite duration together mean the browser has looked
 * at a real stream. A live stream reports `Infinity`, which is not a file — and a decorative
 * background loop is not what anyone came for.
 */
function loaded(player: Player): boolean {
  const video = player.video;
  return (
    !player.decorative &&
    video.readyState >= 1 &&
    Number.isFinite(video.duration) &&
    video.duration > 0
  );
}
