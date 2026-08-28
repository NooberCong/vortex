/**
 * Writing to `storage.session`, which is smaller than it looks.
 *
 * The session area is one pool — 10 MB in Chrome — shared by every key the extension
 * writes, and `set` *rejects* when a write would overflow it. Every other bound in the
 * extension is per tab: a ledger's entries, a tab's ladders. Ten tabs can each be
 * individually well behaved and collectively over the line, so somewhere there has to be
 * a component that knows the pool is finite, and this is it.
 *
 * What that failure did before this module existed is the reason it does. The rejection
 * had no handler, so it surfaced as an uncaught error in the background console; the write
 * was lost; and because the ledger clears its buffer before flushing, the entries went with
 * it. A tab quietly stopped having any headers to hand over, and nothing said so.
 *
 * So an overflowing write is not an error to report but a cache to shrink. The largest
 * keys are dropped and the write is tried once more. Everything large in the session area
 * is a cache by construction — a tab's request ledger, a tab's parsed ladders — and losing
 * one costs a probe, never a download: the daemon probes before anything is committed. The
 * small keys, the settings mirror and the badge's job ids, are below the floor and are
 * never candidates.
 */

import { browser } from "wxt/browser";

/**
 * Below this a key cannot plausibly be the reason a write did not fit, so it is never
 * sacrificed. It is what keeps the settings mirror and the badge out of the reclaim.
 */
const RECLAIM_FLOOR = 4 * 1024;

type Attempt = "stored" | "full" | "refused";

/**
 * What the session area will charge for a value.
 *
 * Chrome measures a key's cost as the length of the JSON it stores, so this is not an
 * estimate — it is the same arithmetic, done before the write instead of after it.
 */
export function measure(value: unknown): number {
  return JSON.stringify(value)?.length ?? 0;
}

/**
 * Writes to the session area, making room first if there is none.
 *
 * Never rejects. `false` means the value is not stored and the caller is holding the only
 * copy — which is worth knowing at exactly one call site (`queue.ts`, where a mirror
 * recorded for a write that did not land would leave the badge asserting a stale count)
 * and worth nothing at the rest, where the value is a cache and a cache miss is the
 * whole cost.
 */
export async function store(items: Record<string, unknown>): Promise<boolean> {
  const first = await attempt(items);
  if (first !== "full") return first === "stored";
  await reclaim(items);
  return (await attempt(items)) === "stored";
}

async function attempt(items: Record<string, unknown>): Promise<Attempt> {
  try {
    await browser.storage.session.set(items);
    return "stored";
  } catch (error) {
    // Chrome says "Session storage quota bytes exceeded"; the MV2 wording is
    // "QUOTA_BYTES quota exceeded". Anything else is a value that cannot be stored at
    // all, and destroying caches over it would turn one bug into two.
    return /quota/i.test(String(error)) ? "full" : "refused";
  }
}

/**
 * Drops the largest sacrificial keys until there is room for `items`.
 *
 * Largest first, rather than oldest first, because the question being answered is which
 * key is *taking the room* and nothing in the area records an age. In practice it picks
 * the ledger of whichever tab has been busiest — which is also, usefully, the tab whose
 * ledger is most able to rebuild itself from the next few requests it makes.
 */
async function reclaim(items: Record<string, unknown>): Promise<void> {
  try {
    // Twice what is being written, so a run of writes against a full area does not
    // enumerate the whole of it once each.
    const wanted = measure(items) * 2;
    const stored = (await browser.storage.session.get(null)) as Record<string, unknown>;

    const sacrificial = Object.entries(stored)
      .filter(([key]) => !(key in items))
      .map(([key, value]) => ({ key, bytes: key.length + measure(value) }))
      .filter(({ bytes }) => bytes >= RECLAIM_FLOOR)
      .sort((a, b) => b.bytes - a.bytes);

    const doomed: string[] = [];
    let freed = 0;
    for (const { key, bytes } of sacrificial) {
      if (freed >= wanted) break;
      doomed.push(key);
      freed += bytes;
    }
    if (doomed.length > 0) await browser.storage.session.remove(doomed);
  } catch {
    // Enumerating or removing failed. The retry will fail too and the caller will be told;
    // there is nothing further to try, and throwing from here would only replace a handled
    // full area with the unhandled rejection this module exists to remove.
  }
}
