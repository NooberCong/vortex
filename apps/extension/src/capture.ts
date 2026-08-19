/**
 * Channel 1 — the `webRequest` observer (03 §The four capture channels).
 *
 * MV3 removed *blocking* `webRequest`, not observational `webRequest`. Vortex never
 * needed to block: it watches, and cancels the resulting download afterwards. Everything
 * this module uses — `onBeforeRequest`, `onSendHeaders`, `onHeadersReceived`,
 * `onCompleted`, and `extraHeaders` for the `Cookie` — is fully intact in MV3.
 *
 * One request arrives as four or five separate events, so the envelope is assembled across
 * them in an in-memory map keyed by `requestId` and committed to the ledger when the
 * request finishes. That map is the one piece of in-flight state the worker holds; if the
 * worker is evicted mid-request the entry is lost, which costs that one request its
 * headers and nothing else.
 */

import type { Browser } from "wxt/browser";
import { browser } from "wxt/browser";

import type { RequestEnvelope } from "@vortex/proto";
import type { HttpHeader } from "./envelope";
import {
  cookieHeader,
  filenameFromDisposition,
  headerValue,
  headers as headerList,
} from "./envelope";
import * as ledger from "./ledger";

/**
 * Which resource types are worth remembering.
 *
 * A page issues hundreds of requests and almost all of them are stylesheets, scripts,
 * fonts and images that will never become a download. The ledger exists to answer one
 * question — "what did the browser send for the URL that just turned into a download, or
 * that just turned out to be a manifest" — and these are the types that can be the answer.
 * Recording the rest would multiply the ledger's size and cost by ten to insure against
 * right-clicking Save on a webfont.
 */
const WORTH_KEEPING = new Set([
  "main_frame",
  "sub_frame",
  "xmlhttprequest",
  "media",
  "object",
  "other",
]);

/** In-flight envelopes, keyed by `requestId`. Discarded when the request ends. */
const inFlight = new Map<string, RequestEnvelope>();
/** A hard ceiling, so a page that opens thousands of connections cannot grow the map. */
const MAX_IN_FLIGHT = 2000;

/** What `onHeadersReceived` learned, so a takeover can size and name the file. */
export interface Observed {
  mimeType?: string;
  contentLength?: number;
  filenameHint?: string;
  disposition?: string;
}

type Sink = (details: {
  url: string;
  tabId: number;
  type: string;
  observed: Observed;
}) => void;

/**
 * Registers the observers. Called synchronously at the top of the background entrypoint —
 * a listener added inside an async callback is silently lost on the next worker wake
 * (03 §Service worker lifetime).
 *
 * `onResponse` is invoked for every response with headers, which is what the manifest
 * sniffer subscribes to.
 */
export function observe(onResponse: Sink): void {
  const filter = { urls: ["<all_urls>"] };

  browser.webRequest.onBeforeRequest.addListener(
    (details): undefined => {
      if (!WORTH_KEEPING.has(details.type)) return undefined;
      if (inFlight.size >= MAX_IN_FLIGHT) inFlight.clear();
      inFlight.set(details.requestId, {
        url: details.url,
        method: details.method,
        headers: [],
        bodyBase64: encodeBody(details.requestBody),
        tabId: details.tabId >= 0 ? details.tabId : undefined,
        pageUrl: pageUrlOf(details),
        capturedAt: stamp(details.timeStamp),
      });
      return undefined;
    },
    filter,
    ["requestBody"],
  );

  browser.webRequest.onSendHeaders.addListener(
    (details) => {
      const envelope = inFlight.get(details.requestId);
      if (!envelope) return;
      envelope.headers = headerList(details.requestHeaders);
      envelope.cookies = cookieHeader(details.requestHeaders);
    },
    filter,
    requestHeaderSpec(),
  );

  browser.webRequest.onHeadersReceived.addListener(
    (details): undefined => {
      const observed = describe(details.responseHeaders);
      const envelope = inFlight.get(details.requestId);
      if (envelope) {
        envelope.mimeType = observed.mimeType;
        envelope.contentLength = observed.contentLength;
        envelope.filenameHint = observed.filenameHint;
        // A redirect chain ends somewhere else; `details.url` is where we are now.
        if (details.url !== envelope.url) envelope.finalUrl = details.url;
      }
      onResponse({
        url: details.url,
        tabId: details.tabId,
        type: details.type,
        observed,
      });
      return undefined;
    },
    filter,
    responseHeaderSpec(),
  );

  browser.webRequest.onCompleted.addListener(
    (details) => {
      const envelope = inFlight.get(details.requestId);
      inFlight.delete(details.requestId);
      if (!envelope) return;
      // A 4xx/5xx body is not worth replaying, and a redirect's own record is superseded
      // by the request it redirected to.
      if (details.statusCode >= 400) return;
      if (details.url !== envelope.url) envelope.finalUrl = details.url;
      ledger.record(envelope);
    },
    filter,
  );

  browser.webRequest.onErrorOccurred.addListener((details) => {
    inFlight.delete(details.requestId);
  }, filter);

  browser.tabs.onRemoved.addListener((tabId) => ledger.forget(tabId));
}

/**
 * The envelope assembled so far for a request that has not finished yet.
 *
 * The ledger is keyed by tab and searched by URL, because by the time a download exists
 * the platform no longer says which request produced it. A response being decided *right
 * now* has no such problem: `requestId` identifies it exactly, so the Firefox front of
 * channel 2 gets the browser's real headers instead of the closest match to them.
 *
 * A copy, because the caller decorates it with what the response said and the observer is
 * not finished with the original.
 */
export function inFlightEnvelope(requestId: string): RequestEnvelope | undefined {
  const envelope = inFlight.get(requestId);
  return envelope ? { ...envelope, headers: [...envelope.headers] } : undefined;
}

/**
 * `extraHeaders` is what makes `Cookie` and `Authorization` visible, and it is a
 * Chrome-only member of the enum. Asking for it where it does not exist throws and takes
 * the whole listener with it; Firefox reports those headers without being asked.
 */
function requestHeaderSpec(): Array<"requestHeaders" | "extraHeaders"> {
  return supportsExtraHeaders(browser.webRequest.OnSendHeadersOptions)
    ? ["requestHeaders", "extraHeaders"]
    : ["requestHeaders"];
}

function responseHeaderSpec(): Array<"responseHeaders" | "extraHeaders"> {
  return supportsExtraHeaders(browser.webRequest.OnHeadersReceivedOptions)
    ? ["responseHeaders", "extraHeaders"]
    : ["responseHeaders"];
}

function supportsExtraHeaders(options: unknown): boolean {
  return (
    typeof options === "object" &&
    options !== null &&
    "EXTRA_HEADERS" in (options as Record<string, unknown>)
  );
}

/** What the response headers say about the body. */
export function describe(list: HttpHeader[] | undefined): Observed {
  const disposition = headerValue(list, "content-disposition");
  const length = headerValue(list, "content-length");
  const parsed = length === undefined ? Number.NaN : Number(length);
  return {
    mimeType: headerValue(list, "content-type")?.split(";")[0]?.trim(),
    contentLength: Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined,
    filenameHint: filenameFromDisposition(disposition),
    disposition,
  };
}

/**
 * `timeStamp` as a whole number of milliseconds.
 *
 * Chrome's `webRequest` timestamps carry sub-millisecond precision — `1787152012345.678`
 * — and `capturedAt` is a `u64` on the wire. `serde_json` will not read a float into an
 * integer field, so an unrounded stamp does not make the envelope slightly wrong: it makes
 * the whole command undeserialisable, and the native host drops it with a warning on a
 * stderr the browser throws away. Every probe and every replayed envelope is built here,
 * so this one coercion is the difference between capture working and capture being
 * silently, completely dead.
 */
function stamp(timeStamp: number | undefined): number {
  return Math.floor(timeStamp || 0) || Date.now();
}

/**
 * The document a request was made from. Firefox states it outright; Chrome gives only the
 * initiating origin, which is still the right `Referer` fallback and the right key for
 * "which page is this".
 */
function pageUrlOf(details: { type: string; url: string; documentUrl?: string; initiator?: string }) {
  if (details.type === "main_frame") return details.url;
  return details.documentUrl ?? details.initiator ?? undefined;
}

/**
 * A POST-initiated download — a form submission that returns a file — replays only if the
 * body goes with it. `formData` is re-encoded rather than passed through because the
 * engine sends bytes, not a dictionary.
 */
function encodeBody(
  body: Browser.webRequest.OnBeforeRequestDetails["requestBody"],
): string | undefined {
  if (!body) return undefined;
  if (body.raw?.length) {
    const chunks = body.raw.map((part) => new Uint8Array(part.bytes ?? new ArrayBuffer(0)));
    const total = chunks.reduce((sum, c) => sum + c.length, 0);
    if (total === 0 || total > 1 << 20) return undefined;
    const flat = new Uint8Array(total);
    let at = 0;
    for (const chunk of chunks) {
      flat.set(chunk, at);
      at += chunk.length;
    }
    let binary = "";
    for (const byte of flat) binary += String.fromCharCode(byte);
    return btoa(binary);
  }
  if (body.formData) {
    const encoded = new URLSearchParams();
    for (const [key, values] of Object.entries(body.formData)) {
      // `FormDataItem` is a string or an `ArrayBuffer`; a file part is the second, and a
      // file part is not something to re-encode into a query string.
      for (const value of values) {
        if (typeof value === "string") encoded.append(key, value);
      }
    }
    const text = encoded.toString();
    return text ? btoa(text) : undefined;
  }
  return undefined;
}

/** Test seam. */
export function __resetForTests(): void {
  inFlight.clear();
}
