/**
 * Vortex's button in the browser's toolbar — the count on it, and opening its popup.
 *
 * ## The count (03 §2 — the acknowledgement)
 *
 * This is the floor of everything Vortex says about a captured download, and it is the
 * floor because it is the only surface that always exists. The in-page receipt needs a
 * page with a content script in it; the popup needs someone to open it. A takeover started
 * from a PDF viewer, from a `file://` page, from a tab that closed behind itself, or from
 * an origin on the denylist's `exclude_matches` has none of those — and a badge still
 * appears.
 *
 * It is also in the right *place*. Both browsers put their own download indicator in the
 * toolbar, a few pixels from the extensions area, and that is where a decade has taught
 * people to look after clicking a link. Vortex takes the browser's indicator away
 * (`takeover.ts` cancels and then erases), so it owes one back in the same corner of the
 * screen rather than in a system notification on another monitor.
 *
 * ## Why it is grey
 *
 * `--attention` is the only colour the tokens allow outside the segment map, and it means
 * something is wrong. A download starting is not wrong. The badge is therefore achromatic
 * like the rest of the chassis (05 §The one rule) — and the fact that a badge is *there
 * at all*, on an icon that normally carries none, is the signal. The one value is written
 * here rather than taken from `@vortex/tokens` because the browser paints this, outside
 * any document: there is no stylesheet to read a token from, and no way to know whether
 * the toolbar behind it is light or dark. So it is the one slate that stays legible on
 * both, with white on top of it.
 */

import { browser } from "wxt/browser";

/** Legible on a light toolbar and on a dark one. Not a token; see the note above. */
const INK = "#5b6577";
const PAPER = "#ffffff";

/** Above this the number stops being worth reading precisely, and stops fitting. */
const MANY = 99;

/**
 * The toolbar button, under whichever name this browser has for it.
 *
 * MV3 renamed `browserAction` to `action` and the WXT `browser` object is the platform's
 * own, not a polyfill — so on the Firefox MV2 build `browser.action` is simply not there.
 * Every method used here has the same signature under both names.
 */
interface Surface {
  setBadgeText(details: { text: string }): unknown;
  setBadgeBackgroundColor(details: { color: string }): unknown;
  /** Chrome 110+ and Firefox 63+. Absent elsewhere, where the browser picks a contrast. */
  setBadgeTextColor?(details: { color: string }): unknown;
  setTitle(details: { title: string }): unknown;
  /**
   * Chrome 127+ and Firefox 118+. See {@link open} for what its absence costs.
   *
   * Firefox's manifest declares `strict_min_version: "115.0"`, so this really is missing
   * on browsers Vortex supports — it is not a defensive `?`.
   */
  openPopup?(): Promise<void>;
}

function surface(): Surface | undefined {
  const api = browser as unknown as { action?: Surface; browserAction?: Surface };
  return api.action ?? api.browserAction;
}

/**
 * Draws `count` unfinished transfers, or clears the badge when there are none.
 *
 * Never throws. A toolbar button is not something any decision depends on, and a browser
 * that refuses to paint one must not take a capture path down with it.
 */
export async function paint(count: number): Promise<void> {
  const button = surface();
  if (!button) return;
  try {
    await button.setBadgeText({ text: count > 0 ? text(count) : "" });
    await button.setTitle({ title: title(count) });
    if (count === 0) return;
    // Set with the text rather than once at startup: MV3 evicts the worker and a colour
    // set in a previous life is not guaranteed to have survived it.
    await button.setBadgeBackgroundColor({ color: INK });
    await button.setBadgeTextColor?.({ color: PAPER });
  } catch {
    // An unloaded browser action, a window closing, an API this build does not have.
  }
}

function text(count: number): string {
  return count > MANY ? `${MANY}+` : String(count);
}

/**
 * What the hover says, because a number on its own is a riddle.
 *
 * "Transfer" rather than "download": the count includes a job that is paused, queued or
 * waiting for an answer, and none of those is downloading anything right now.
 */
function title(count: number): string {
  if (count === 0) return "Vortex";
  return count === 1 ? "Vortex — 1 transfer" : `Vortex — ${count} transfers`;
}

/**
 * Opens the popup, and says whether it managed to.
 *
 * The receipt is the only caller. A card that names a file and cannot be followed is a
 * dead end — the popup is where that file appears in a list with a progress bar, one
 * click from the app — so the click on the card asks for the same panel the toolbar icon
 * would have given, which is exactly what the receipt has been telling people to go and
 * press.
 *
 * ## Why the return value exists
 *
 * `openPopup` is the youngest API in this file by a decade. Chrome only made it generally
 * available in 127 (it was policy-installed extensions only from 118, and absent in stable
 * before that), Firefox required a user-input handler until 118 — and a message from a
 * content script is not one, because activation does not survive the trip to the worker.
 * Against `strict_min_version: "115.0"` those are live cases, not hypotheticals.
 *
 * So this reports rather than throws, and the caller has somewhere else to go. It is a
 * plain `false` for both "this browser has no such method" and "it refused", because the
 * caller does nothing different with the two.
 */
export async function open(): Promise<boolean> {
  const button = surface();
  if (!button?.openPopup) return false;
  try {
    await button.openPopup();
    return true;
  } catch {
    // No focused window, a popup already open, or a browser that wants a gesture it
    // cannot see. All of them mean the same thing here: ask somewhere else.
    return false;
  }
}
