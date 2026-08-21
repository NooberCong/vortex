/**
 * The extension's own internal messages.
 *
 * Everything that crosses to `vortexd` is a generated `Command`/`Event` from
 * `@vortex/proto`. These are the shapes that stay inside the browser — background to
 * overlay, page to content script — and they are deliberately few. A message here means
 * one of the halves cannot see something the other can: the overlay cannot reach the
 * daemon, and the page's `MediaSource` cannot be observed from an isolated world.
 */

import { browser } from "wxt/browser";

import type { JobId, JobView, MediaCandidate, MediaSelection } from "@vortex/proto";
import type { Link } from "./host";

/** Background → content script. */
export type ToPage =
  | { kind: "candidates"; candidates: MediaCandidate[] }
  /**
   * Re-acquire `url` in page context (03 §Handoff 3). The content script answers with the
   * URL the browser ended up at, or `null` if it could not get one.
   */
  | { kind: "renew"; url: string }
  /**
   * A download on this page has just been moved to Vortex (03 §2).
   *
   * The receipt the takeover owes the user. Both capture channels erase the browser's own
   * download before refetching it, so from the page's side a click produced a flicker and
   * then nothing at all — and the honest reading of that is "it failed", which is why
   * people click again and get the file twice. This is the sentence that stops them.
   */
  | { kind: "captured"; filename: string };

/** Content script → background. */
export type FromPage =
  | { kind: "ready" }
  /**
   * `tabId` is for the popup, which is not in a tab and so has no `sender.tab` for the
   * background to read the page off. A content script leaves it out and is believed by
   * the browser instead — a page cannot nominate a tab it is not in.
   */
  | { kind: "download"; selection: MediaSelection; pageTitle: string; tabId?: number }
  /**
   * Turn capture on or off for one origin (03 §The overlay).
   *
   * One message rather than a `dismiss` and an `undismiss`, because there is one piece of
   * state and two places that set it: the overlay's "Not on this site", which can only
   * ever turn it off, and the popup's switch, which is the only way back — the overlay
   * cannot offer one, having removed itself from the page.
   */
  | { kind: "siteCapture"; origin: string; on: boolean }
  /**
   * A real player has been loaded on this page long enough that every other channel
   * would have produced a ladder by now, and none did (03 §5). The page URL is offered
   * to the daemon's extractor, which is the only thing left that can look at it.
   */
  | { kind: "orphan"; pageUrl: string }
  /**
   * The same signal as `orphan`, raised from a **subframe** (03 §5, embedded players).
   *
   * It carries no URL, and that is the point. The top document is entitled to name itself
   * because it *is* the page; a subframe is the one sender that is routinely someone
   * else's code, and a message that let it nominate a URL would let it choose what the
   * daemon's extractor goes and fetches. The background reads `sender.tab.url` instead,
   * which the browser fills in and the frame cannot influence.
   */
  | { kind: "framePlayer" }
  /** What the popup needs to draw itself, for the tab it was opened over. */
  | { kind: "popupState"; tabId: number }
  /**
   * The transfer list again, while the popup is open.
   *
   * Polled rather than subscribed. `Summary` frames arrive at 2 Hz for every job in the
   * queue and would keep the MV3 service worker awake for as long as anything was
   * downloading — a background page that never sleeps, to animate a panel nobody is
   * looking at (03 §Service worker lifetime). A popup lives for a few seconds and asks
   * for itself.
   */
  | { kind: "jobs" }
  /**
   * "Open Vortex, and put this job in front of me."
   *
   * The extension cannot reach the window — they are two processes that share a daemon and
   * nothing else — so this goes out as a `Reveal` and the daemon starts or raises the app.
   * `null` is the bare "open Vortex" the popup's footer asks for.
   */
  | { kind: "reveal"; job: JobId | null }
  /**
   * "Show me the panel" — the takeover receipt has been clicked (03 §2b).
   *
   * It carries no job, and it is not a `reveal`: the card names a file the daemon has only
   * just been told about, and the id for it arrives afterwards on a `JobAdded` broadcast
   * that this message would have to race. What the click actually means is *the smaller*
   * of the two asks — show me the list — and the popup is where that list already is,
   * one click from the app for anyone who wants more.
   *
   * Only the background can honour it. `action.openPopup` is an extension API and a
   * content script is not the extension; a page asking for it directly is a page opening
   * browser UI, which is why the platform does not offer it there.
   */
  | { kind: "popup" };

/**
 * The popup's answer to `popupState`.
 *
 * Deliberately everything at once. The popup is opened, read and dismissed in a couple of
 * seconds; three round trips to fill in three panels would show it assembling itself.
 */
export interface PopupState {
  /** `null` when the tab has no origin to speak of — a new tab, a PDF, a settings page. */
  origin: string | null;
  pageTitle: string;
  /** The user has switched Vortex off for this origin. */
  optedOut: boolean;
  /** Capture is off everywhere, from the app's settings. */
  captureOff: boolean;
  /**
   * Whether the rest of Vortex is there, and if not, which "not".
   *
   * Not a boolean, because the popup's whole job in the unhappy case is to say the true
   * thing: "start the app" and "you do not have the app" are different sentences with
   * different buttons under them (`src/host.ts`).
   */
  daemon: Link;
  candidates: MediaCandidate[];
  /**
   * Everything in the daemon's queue, so the first paint is not an empty panel that fills
   * in a moment later. `null` is "the daemon did not answer", which is a different thing
   * from an empty queue and is drawn differently.
   */
  jobs: JobView[] | null;
}

/**
 * MAIN-world hook → isolated content script, over `window.postMessage`.
 *
 * Metadata only, never bytes (03 §4). Its real value is answering "is a video actually
 * playing right now, and which one", so the overlay can be right instead of eager.
 */
export interface MseSignal {
  /** Discriminator, because `window.postMessage` is a channel anyone can write to. */
  source: "vortex-mse";
  /** Codec strings from `addSourceBuffer` — the ladder needs them and the manifest may be hidden. */
  codecs: string[];
  /** URLs the page fetched that look like segments or manifests. */
  urls: string[];
  /** Whether any `SourceBuffer` has actually been appended to. */
  playing: boolean;
}

/** Content script → background, relaying the hook. */
export interface MseReport {
  kind: "mse";
  signal: MseSignal;
}

export type Internal = FromPage | MseReport;

/**
 * Sends one internal message, and never throws either way.
 *
 * The `try` is not belt-and-braces. Once the extension has been reloaded under an open
 * page, `sendMessage` throws *synchronously*, before it ever returns a promise — so the
 * `.catch` is not attached to anything. That is the difference between a silent no-op and
 * an uncaught "Extension context invalidated" in the console of every tab the user has
 * open, for as long as they leave it open.
 */
export function post(message: Internal): Promise<unknown> {
  try {
    return browser.runtime.sendMessage(message).catch(() => undefined);
  } catch {
    return Promise.resolve(undefined);
  }
}

/**
 * Sends one message the other way, to a content script, and never throws either.
 *
 * The common failure is not a failure: plenty of tabs have no content script in them. The
 * DRM denylist is in `exclude_matches`, a PDF viewer and an internal page are not pages
 * this extension is in at all, and a tab can navigate away between the decision and the
 * message. Every one of those is "nobody to tell", which is not worth reporting.
 */
export async function tell(tabId: number, message: ToPage): Promise<void> {
  try {
    await browser.tabs.sendMessage(tabId, message);
  } catch {
    // No content script in that tab. See above.
  }
}
