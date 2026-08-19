/**
 * The per-tab request ledger (03 §1).
 *
 * Re-issuing a browser request from another process fails when a header is missing, and
 * the header that is missing is almost always `Referer`. So the extension keeps what the
 * browser actually sent, and the engine replays it verbatim. This ledger is that memory.
 *
 * Two constraints shape the implementation, and they pull in opposite directions:
 *
 * - **The MV3 worker holds no state.** It is evicted after ~30 s idle, so anything that
 *   has to survive lives in `browser.storage.session`.
 * - **A page makes hundreds of requests.** A storage write per `webRequest` event would be
 *   thousands of writes per page load, which is a measurable tax on every browsing session
 *   whether or not the user ever downloads anything.
 *
 * The resolution is a write-behind buffer: entries land in memory and are flushed to
 * session storage a beat later, and the flush is forced at the two moments the ledger is
 * about to be read. A worker evicted inside that window loses the unflushed tail — but the
 * worker is only evicted after 30 s of idleness, and the window is 250 ms of activity, so
 * the two barely intersect. The alternative costs every user real battery to close a gap
 * that costs one download its headers.
 */

import { browser } from "wxt/browser";

import type { RequestEnvelope } from "@vortex/proto";

/** Per 03 §1. Old entries are evicted first; a tab that browses does not grow forever. */
const MAX_PER_TAB = 500;
const FLUSH_DELAY = 250;

const key = (tabId: number) => `ledger:${tabId}`;

/** Tabs with unflushed entries, and the timer that will write them. */
const pending = new Map<number, RequestEnvelope[]>();
let timer: ReturnType<typeof setTimeout> | null = null;

/**
 * Storage is read-modify-write, and `webRequest` fires faster than a round trip. Without
 * serialisation two flushes interleave and the second silently discards the first's
 * entries. One promise chain per tab is the whole fix.
 */
const writes = new Map<number, Promise<void>>();

function serialise(tabId: number, work: () => Promise<void>): Promise<void> {
  const next = (writes.get(tabId) ?? Promise.resolve()).then(work, work);
  writes.set(tabId, next);
  void next.finally(() => {
    if (writes.get(tabId) === next) writes.delete(tabId);
  });
  return next;
}

async function read(tabId: number): Promise<RequestEnvelope[]> {
  const stored = await browser.storage.session.get(key(tabId));
  const entries = stored[key(tabId)];
  return Array.isArray(entries) ? (entries as RequestEnvelope[]) : [];
}

/** Records one completed request against its tab. Cheap: memory only until the flush. */
export function record(envelope: RequestEnvelope): void {
  const tabId = envelope.tabId;
  if (tabId === undefined || tabId === null || tabId < 0) return;

  const queue = pending.get(tabId) ?? [];
  queue.push(envelope);
  pending.set(tabId, queue);

  if (timer === null) {
    timer = setTimeout(() => {
      timer = null;
      void flush();
    }, FLUSH_DELAY);
  }
}

/** Writes every buffered entry. Awaited before any read, so a lookup never races a write. */
export async function flush(): Promise<void> {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  const batches = [...pending.entries()];
  pending.clear();

  for (const [tabId, additions] of batches) {
    void serialise(tabId, async () => {
      const merged = [...(await read(tabId)), ...additions];
      // Newest wins: an expiring signed URL is re-minted under the same path, and the
      // fresh one is the only one worth replaying.
      const trimmed = merged.slice(Math.max(0, merged.length - MAX_PER_TAB));
      await browser.storage.session.set({ [key(tabId)]: trimmed });
    });
  }

  // Wait on every tab's chain, not just the batches this call happened to drain.
  // A concurrent `flush` may already have taken the entries we are about to read and
  // still be writing them; returning before *that* lands is how a lookup sees a ledger
  // that is missing the request it was called about.
  await Promise.all([...writes.values()]);
}

/**
 * The envelope the browser used for `url` in this tab, newest first.
 *
 * Matching is exact on the URL, then on the URL with its query stripped. The second pass
 * catches a CDN that appends a fresh signature per request: the path is the identity, and
 * a stale signature is still better than no headers at all — the engine probes before
 * anything is committed, so a miss costs one probe rather than a lost download.
 */
export async function lookup(
  tabId: number | undefined,
  url: string,
): Promise<RequestEnvelope | null> {
  if (tabId === undefined || tabId < 0) return null;
  await flush();
  const entries = await read(tabId);

  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i]!;
    if (entry.url === url || entry.finalUrl === url) return entry;
  }
  const stem = withoutQuery(url);
  if (stem === null) return null;
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i]!;
    if (withoutQuery(entry.url) === stem || withoutQuery(entry.finalUrl) === stem) {
      return entry;
    }
  }
  return null;
}

/** The most recent top-level document request in a tab — the page a download came from. */
export async function page(tabId: number | undefined): Promise<RequestEnvelope | null> {
  if (tabId === undefined || tabId < 0) return null;
  await flush();
  const entries = await read(tabId);
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i]!;
    if (entry.pageUrl && entry.url === entry.pageUrl) return entry;
  }
  return null;
}

/** A closed tab's ledger is dead weight and, until it is dropped, a retained cookie header. */
export function forget(tabId: number): void {
  pending.delete(tabId);
  void serialise(tabId, () => browser.storage.session.remove(key(tabId)));
}

function withoutQuery(url: string | undefined | null): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    return null;
  }
}

/** Test seam: drops the in-memory buffer without touching storage. */
export function __resetForTests(): void {
  if (timer !== null) clearTimeout(timer);
  timer = null;
  pending.clear();
  writes.clear();
}
