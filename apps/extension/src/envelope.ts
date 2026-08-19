/**
 * Turning what the browser did into a [`RequestEnvelope`] the engine can replay.
 *
 * The engine already refuses to replay hop-by-hop and range headers (`vortex-engine`'s
 * `RESERVED` list), so nothing is filtered here. One list, in one place, on the side that
 * actually issues the request — a second copy in the extension would drift and the drift
 * would show up as a 403 nobody could explain.
 *
 * `Cookie` is the exception, and it is moved rather than dropped: the protocol gives it a
 * field of its own because the daemon keeps credentials in memory and never lets them
 * reach `vortex.db`, a log line, or a `.vxpart.meta` (01 §Security boundaries). A cookie
 * hidden among the ordinary headers would quietly defeat that.
 */

import type { Browser } from "wxt/browser";
import { browser } from "wxt/browser";

import type { RequestEnvelope } from "@vortex/proto";

/** The platform's own header shape, so nothing here has to be kept in step by hand. */
export type HttpHeader = Browser.webRequest.HttpHeader;

/** Header list minus `Cookie`, in the order the browser sent it. */
export function headers(list: HttpHeader[] | undefined): Array<[string, string]> {
  return (list ?? [])
    .filter((h) => h.value !== undefined && !isCookie(h.name))
    .map((h) => [h.name, h.value!] as [string, string]);
}

/** The `Cookie` header the browser actually sent, which is the one worth having. */
export function cookieHeader(list: HttpHeader[] | undefined): string | undefined {
  const found = (list ?? []).find((h) => isCookie(h.name));
  return found?.value || undefined;
}

const isCookie = (name: string) => name.toLowerCase() === "cookie";

/**
 * Reconstructs a cookie header from the cookie store.
 *
 * Only used when the observed request had none to copy — a download the browser started
 * without a matching `webRequest` record, or a Firefox build that withheld the header.
 * Reconstruction is strictly worse than observation: it cannot see which cookies the
 * browser would actually have sent for this request's partition and same-site context. It
 * is here because some header beats none, and the daemon probes before committing.
 */
export async function cookiesFromStore(url: string): Promise<string | undefined> {
  try {
    const jar = await browser.cookies.getAll({ url });
    if (jar.length === 0) return undefined;
    return jar.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    // No `cookies` permission, or a URL the store will not answer for.
    return undefined;
  }
}

/** The page a request belongs to. Used for naming, and for renewal (03 §Handoff). */
export async function describeTab(
  tabId: number | undefined,
): Promise<{ pageUrl?: string; pageTitle?: string }> {
  if (tabId === undefined || tabId < 0) return {};
  try {
    const tab = await browser.tabs.get(tabId);
    return { pageUrl: tab.url || undefined, pageTitle: tab.title || undefined };
  } catch {
    // The tab closed between the download starting and this call. Not an error.
    return {};
  }
}

/**
 * Is this response the browser's own verdict that it is a file?
 *
 * `Content-Disposition: attachment` is the one header whose meaning is not a guess: it
 * says the body is to be saved, not rendered, whatever its type. Everything else that
 * becomes a download — an `application/octet-stream` navigation, a PDF where the built-in
 * viewer is off — depends on browser settings this code cannot see, so pre-empting on
 * those would mean cancelling requests the browser would have displayed.
 *
 * The disposition *type* is the token before the first `;`. Matching the header as a
 * substring would make `inline; filename="attachment.pdf"` a download.
 */
export function isAttachment(value: string | undefined): boolean {
  return value?.split(";")[0]?.trim().toLowerCase() === "attachment";
}

/**
 * `attachment; filename*=UTF-8''Ren%C3%A9.pdf` → `René.pdf`.
 *
 * RFC 5987's `filename*` wins over `filename` when both are present, which is the whole
 * point of it: the plain form is the ASCII fallback for clients that cannot do better.
 */
export function filenameFromDisposition(value: string | undefined): string | undefined {
  if (!value) return undefined;

  const extended = /filename\*\s*=\s*([^;]+)/i.exec(value);
  if (extended) {
    // `charset'language'percent-encoded`. The charset is read but not honoured: it is
    // UTF-8 in everything written this century, and mis-decoding a rare ISO-8859-1 header
    // is a wrong name, while failing here is no name at all.
    const encoded = extended[1]!.trim().split("'")[2] ?? "";
    try {
      const decoded = decodeURIComponent(encoded);
      if (decoded) return decoded;
    } catch {
      // A malformed percent-escape falls through to the plain form below.
    }
  }

  const plain = /filename\s*=\s*("([^"]*)"|[^;]+)/i.exec(value);
  const name = (plain?.[2] ?? plain?.[1] ?? "").trim();
  return name || undefined;
}

/** Case-insensitive lookup over a header list. */
export function headerValue(
  list: HttpHeader[] | undefined,
  name: string,
): string | undefined {
  return (list ?? []).find((h) => h.name.toLowerCase() === name.toLowerCase())?.value;
}

/**
 * A last-resort envelope for a URL nothing in the ledger explains.
 *
 * The browser can start a download from a context `webRequest` never reported — a
 * `blob:` navigation resolved elsewhere, a right-click Save on something served from
 * cache, or a request made before the worker woke. Rebuilding from the cookie store and
 * the tab's URL is thin, but the probe decides whether it is enough.
 */
export async function synthesise(
  url: string,
  tabId: number | undefined,
): Promise<RequestEnvelope> {
  const [cookies, tab] = await Promise.all([cookiesFromStore(url), describeTab(tabId)]);
  const list: Array<[string, string]> = [];
  if (tab.pageUrl) {
    // The single most common cause of a handoff 403 is a missing `Referer`.
    list.push(["Referer", tab.pageUrl]);
    try {
      list.push(["Origin", new URL(tab.pageUrl).origin]);
    } catch {
      // An `about:` or `moz-extension:` page has no useful origin.
    }
  }
  return {
    url,
    method: "GET",
    headers: list,
    cookies,
    tabId,
    ...tab,
    capturedAt: Date.now(),
  };
}
