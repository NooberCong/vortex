/**
 * Channel 2, Firefox form — pre-emption (03 §2).
 *
 * `takeover.ts` lets the browser start a download and then cancels it. That is the only
 * thing MV3 permits, and it has a cost the user can see: a file small enough to finish
 * inside the takeover decision is already written before Vortex gets to ask for it.
 * Firefox kept blocking `webRequest`, and uniquely allows a blocking listener to answer
 * with a *promise*, so on Firefox the decision can happen one step earlier:
 *
 * ```text
 * onHeadersReceived(response)
 *    ├── not a navigation, not a GET, not 200? → let it through
 *    ├── not `Content-Disposition: attachment`? → let it through
 *    ├── (from here the response is suspended)
 *    ├── denylisted, opted out, too small?      → let it through
 *    ├── does the daemon get bytes?             → probe; if not, let it through
 *    ├── budget spent waiting?                  → let it through
 *    └── cancel + Submit
 * ```
 *
 * No `DownloadItem` is created, nothing is written to disk, nothing appears in the
 * download list, and there is no race to lose. **Every branch that is not the last one
 * lets the response through to the browser**, where channel 2 sees it as an ordinary
 * download and gets its own turn — so this channel can only ever be an improvement on the
 * one behind it, never a replacement for it.
 *
 * Two deliberate narrowings, both about the same risk. Cancelling a request the browser
 * would have *rendered* breaks the page, and that is a failure channel 2 structurally
 * cannot have, because it acts on the browser's verdict instead of predicting it:
 *
 * - **Navigations only.** A page that `fetch`es an endpoint returning an attachment and
 *   builds a blob from it is not downloading anything, and cancelling that is a broken
 *   site. A *navigation* that returns an attachment is always a download.
 * - **GET only.** The daemon re-issues the request, and the browser's POST has already
 *   reached the server. Only a GET is safe to send twice.
 */

import { browser } from "wxt/browser";

import type { JobSpec, RequestEnvelope } from "@vortex/proto";
import { BLOCKING_WEBREQUEST } from "./build";
import * as capture from "./capture";
import type { HttpHeader } from "./envelope";
import { describeTab, headerValue, isAttachment, synthesise } from "./envelope";
import * as handoff from "./handoff";
import * as host from "./host";

/**
 * How long the response may stay suspended.
 *
 * A ceiling on the whole decision rather than a timeout on one step of it, because what
 * the user experiences is the total: a click that appears to do nothing. Deliberately far
 * shorter than the probe's ordinary reply timeout — a daemon that has not answered in
 * this long is one the user is better off downloading without.
 */
const DECISION_BUDGET = 1500;

/**
 * The request types that become downloads rather than page content.
 *
 * `other` is not here. It would catch a right-click Save As, and also a beacon, a
 * prefetch, and whatever the platform files under it next release; those fall through to
 * channel 2, which does not have to guess.
 */
const NAVIGATIONS = new Set(["main_frame", "sub_frame"]);

/** What this channel reads off a response. Firefox supplies all of it. */
interface Response {
  requestId: string;
  url: string;
  method: string;
  type: string;
  statusCode: number;
  tabId: number;
  responseHeaders?: HttpHeader[];
  documentUrl?: string;
  originUrl?: string;
  incognito?: boolean;
}

export function watch(): void {
  // Absent from the MV3 bundle entirely: a build that cannot block should not carry the
  // code that would.
  if (!BLOCKING_WEBREQUEST) return;

  // Returning a promise from a blocking listener is the Firefox extension the whole
  // channel rests on, and the shared type package describes Chrome's blocking API, where
  // the answer has to be synchronous. The cast is that difference, stated once.
  const attach = browser.webRequest.onHeadersReceived.addListener as unknown as (
    listener: (details: Response) => Promise<{ cancel?: boolean }> | undefined,
    filter: { urls: string[] },
    extra: string[],
  ) => void;

  try {
    attach(decide, { urls: ["<all_urls>"] }, ["blocking", "responseHeaders"]);
  } catch {
    // No `webRequestBlocking` in this browser. Channel 2 covers everything this would
    // have, so a missing pre-emption costs an optimisation and not a download — and
    // swallowing it here keeps the rest of the background's listeners registering.
  }
}

/**
 * Synchronous first, and only then a promise.
 *
 * Returning a promise suspends the response, so everything decidable from the headers
 * alone is decided before one is returned. Almost every response a browser receives
 * leaves here on the first line.
 */
function decide(details: Response): Promise<{ cancel?: boolean }> | undefined {
  if (!worthStalling(details)) return undefined;
  return commit(details);
}

async function commit(details: Response): Promise<{ cancel?: boolean }> {
  const spec = await Promise.race([plan(details), expire()]);
  // `{}` is "proceed unchanged". The browser downloads it, and channel 2 gets its turn.
  if (!spec) return {};

  host.send({ cmd: "submit", spec });
  return { cancel: true };
}

/** Everything decidable from the response headers, with nothing suspended yet. */
function worthStalling(details: Response): boolean {
  if (!NAVIGATIONS.has(details.type)) return false;
  if (details.method !== "GET") return false;
  // A redirect has not arrived anywhere yet, and an error body is not a file.
  if (details.statusCode !== 200) return false;
  return isAttachment(headerValue(details.responseHeaders, "content-disposition"));
}

/**
 * The job to submit, or `null` for "leave it to the browser".
 *
 * Note what is *not* here: a `Ping`. Channel 2 pings before it cancels because the cancel
 * is destructive and the probe comes after it in time. Here the probe is the only thing
 * that can trigger a cancel, and a probe the daemon answered is proof of life strictly
 * stronger than a ping — one round trip instead of two, on the path where the user is
 * waiting for it.
 */
async function plan(details: Response): Promise<JobSpec | null> {
  const observed = capture.describe(details.responseHeaders);
  const from = details.documentUrl || details.originUrl;

  const allowed = await handoff.wanted({
    url: details.url,
    referrer: from,
    size: observed.contentLength,
    incognito: details.incognito,
  });
  if (!allowed) return null;

  const envelope = await compose(details, observed, from);
  if (!(await handoff.reachesBytes(envelope, DECISION_BUDGET))) return null;

  return {
    envelope,
    // No filename: `onDeterminingFilename` never ran, so there is no browser-resolved name
    // to honour, and the daemon derives a better one from the disposition (04 §8).
    priority: "Normal",
    startPaused: false,
  };
}

/**
 * The exact request the browser made, decorated with what the response said.
 *
 * `requestId` finds it outright — this is the one channel that never has to match a
 * download back to a request by URL. The fallback still matters: `webRequest` may have
 * been listening for less time than this response took to arrive.
 */
async function compose(
  details: Response,
  observed: capture.Observed,
  from: string | undefined,
): Promise<RequestEnvelope> {
  const tabId = details.tabId >= 0 ? details.tabId : undefined;
  const envelope =
    capture.inFlightEnvelope(details.requestId) ?? (await synthesise(details.url, tabId));

  // `details.url` is where the redirect chain actually ended.
  if (details.url !== envelope.url) envelope.finalUrl = details.url;
  if (observed.mimeType) envelope.mimeType = observed.mimeType;
  if (observed.contentLength !== undefined) envelope.contentLength = observed.contentLength;
  if (observed.filenameHint) envelope.filenameHint = observed.filenameHint;
  envelope.tabId ??= tabId;

  if (from && !envelope.headers.some(([key]) => key.toLowerCase() === "referer")) {
    envelope.headers.push(["Referer", from]);
  }

  // For a navigation the observer recorded the file's own URL as the page, because at
  // `onBeforeRequest` that is what a `main_frame` request is. The response says otherwise:
  // the tab never left the page the user clicked from, and that page is what names a file
  // whose URL is a hash (04 §8).
  const tab = await describeTab(tabId);
  if (from) envelope.pageUrl = from;
  envelope.pageTitle ??= tab.pageTitle;
  return envelope;
}

/** Resolves to `null` once the response has been held long enough. */
function expire(): Promise<null> {
  return new Promise((resolve) => setTimeout(() => resolve(null), DECISION_BUDGET));
}
