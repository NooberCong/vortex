/**
 * URL renewal (03 §Handoff 3, 02 §6).
 *
 * A signed CDN URL expires thirty seconds after it is minted. The engine notices —
 * a 403 on a range it was already reading is unambiguous — pauses on the existing block
 * bitmap, and asks for a fresh one. This is the half that answers.
 *
 * The mechanism is deliberately the simplest thing that can work: re-request the URL from
 * inside the page that produced it, with the page's own credentials, and follow the
 * redirect. Whatever the browser lands on is the fresh URL. It works because expiry is
 * nearly always implemented as a redirect from a stable path to a signed one, and because
 * the page's cookies and `Referer` are exactly what the signer wants to see.
 *
 * The good part is what happens next. That re-request goes through `webRequest` like any
 * other, so by the time the content script answers, the ledger already holds a complete
 * envelope for the new URL — real headers, real cookies, captured rather than
 * reconstructed. Renewal costs one fetch and yields a better envelope than the original
 * takeover had.
 *
 * When it fails, nothing happens. The job stays paused and resumable, and the desktop app
 * offers "Reopen the page to continue" — never a dead end, and never a silent retry loop
 * against a URL that will keep saying no.
 */

import { browser } from "wxt/browser";

import type { JobId, RenewalHint, RequestEnvelope } from "@vortex/proto";
import { synthesise } from "./envelope";
import * as host from "./host";
import * as ledger from "./ledger";
import type { ToPage } from "./messages";

/** How long the page gets to re-acquire the URL before we give up on this attempt. */
const RENEW_TIMEOUT = 10_000;

export function listen(): void {
  host.onEvent((event) => {
    if (event.event !== "urlExpired") return;
    void renew(event.job, event.hint);
  });
}

async function renew(job: JobId, hint: RenewalHint): Promise<void> {
  const tabId = await locate(hint);
  if (tabId === undefined) return;

  const fresh = await ask(tabId, hint.url);
  if (!fresh) return;

  const envelope = await envelopeFor(fresh, tabId, hint);
  host.send({ cmd: "renewedUrl", job, envelope });
}

/**
 * The tab that can mint a new URL.
 *
 * The hint's own tab id first, because that is where the URL came from. Failing that, any
 * tab still showing the originating page — the user may have reloaded, or the browser may
 * have restarted and renumbered every tab it restored.
 */
async function locate(hint: RenewalHint): Promise<number | undefined> {
  if (hint.tabId !== undefined && hint.tabId !== null && hint.tabId >= 0) {
    try {
      const tab = await browser.tabs.get(hint.tabId);
      if (tab?.id !== undefined) return tab.id;
    } catch {
      // Closed, or renumbered by a session restore.
    }
  }
  if (!hint.pageUrl) return undefined;
  try {
    const tabs = await browser.tabs.query({ url: hint.pageUrl });
    return tabs[0]?.id;
  } catch {
    return undefined;
  }
}

/** Asks the page to re-acquire the URL. `null` means it could not. */
async function ask(tabId: number, url: string): Promise<string | null> {
  const message: ToPage = { kind: "renew", url };
  const answer = await Promise.race([
    browser.tabs.sendMessage(tabId, message).catch(() => null),
    new Promise<null>((resolve) => setTimeout(() => resolve(null), RENEW_TIMEOUT)),
  ]);
  return typeof answer === "string" && answer.length > 0 ? answer : null;
}

/**
 * The envelope for the renewed URL — captured, if the re-request made it into the ledger.
 *
 * The content script's `fetch` resolves before `onCompleted` necessarily has, so a miss
 * here is a race rather than a failure, and the synthesised fallback still carries the
 * page's `Referer` and cookies.
 */
async function envelopeFor(
  url: string,
  tabId: number,
  hint: RenewalHint,
): Promise<RequestEnvelope> {
  const captured = await ledger.lookup(tabId, url);
  const envelope = captured ?? (await synthesise(url, tabId));
  envelope.url = url;
  envelope.finalUrl = undefined;
  envelope.tabId = tabId;
  envelope.pageUrl ??= hint.pageUrl ?? undefined;
  envelope.capturedAt = Date.now();
  return envelope;
}
