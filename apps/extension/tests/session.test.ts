import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import * as session from "@/src/session";

/**
 * The finite session area.
 *
 * `storage.session` is one ~10 MB pool for the whole extension, and every bound above it
 * is per tab. So the interesting case is not the write that fits — it is the write that
 * does not, which used to surface as an uncaught rejection in the background console and
 * a tab that had silently stopped remembering anything. The properties worth pinning are
 * about what gets sacrificed when the pool is full, because getting that wrong is how a
 * memory-pressure bug becomes a data-loss bug:
 *
 * - the write **succeeds** on the second attempt rather than being reported;
 * - what is dropped is a **cache**, never the key being written and never the small keys
 *   that could not have been the reason it did not fit;
 * - and nothing here **rejects**, whatever the storage layer does.
 */

/** The wording Chrome uses. The MV2 phrasing differs; both contain "quota". */
const FULL = new Error("Session storage quota bytes exceeded. Values were not stored.");

const big = (size: number) => ({ blob: "x".repeat(size) });

/** Fails the next `failures` writes with `error`, then lets storage behave normally. */
function refuse(failures: number, error: Error = FULL): void {
  const real = browser.storage.session.set.bind(browser.storage.session);
  vi.spyOn(browser.storage.session, "set").mockImplementation((async (items: never) => {
    if (failures-- > 0) throw error;
    return real(items);
  }) as never);
}

const stored = async (): Promise<Record<string, unknown>> =>
  (await browser.storage.session.get(null)) as Record<string, unknown>;

describe("writing to a session area that may be full", () => {
  beforeEach(() => {
    fakeBrowser.reset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("writes, and says it wrote", async () => {
    expect(await session.store({ settings: { enableCapture: true } })).toBe(true);
    expect((await stored()).settings).toEqual({ enableCapture: true });
  });

  it("drops the largest cache and gets the write through", async () => {
    await browser.storage.session.set({
      "ledger:1": big(20_000),
      "ledger:2": big(8_000),
    });
    refuse(1);

    expect(await session.store({ "ledger:3": big(5_000) })).toBe(true);
    const after = await stored();
    // The biggest went, and only the biggest: freeing twice the incoming write is enough.
    expect(after["ledger:1"]).toBeUndefined();
    expect(after["ledger:2"]).toBeDefined();
    expect(after["ledger:3"]).toBeDefined();
  });

  it("never sacrifices the key it is writing", async () => {
    // The obvious way to get this wrong is to reclaim by size and find that the biggest
    // key in the area is the one whose new value is what does not fit.
    await browser.storage.session.set({ "ledger:7": big(40_000), "ledger:9": big(30_000) });
    refuse(1);

    expect(await session.store({ "ledger:7": big(20_000) })).toBe(true);
    const after = await stored();
    expect(after["ledger:7"]).toEqual(big(20_000));
    expect(after["ledger:9"]).toBeUndefined();
  });

  it("leaves the small keys alone, and reports the failure instead", async () => {
    // Nothing here is big enough to have been the reason a write did not fit, so there is
    // nothing to reclaim — and the settings mirror and the badge's job ids are exactly the
    // two things that must never be traded for room.
    await browser.storage.session.set({ settings: { enableCapture: true }, activeJobs: [1, 2] });
    refuse(Infinity);

    expect(await session.store({ "ledger:1": big(5_000) })).toBe(false);
    const after = await stored();
    expect(after.settings).toEqual({ enableCapture: true });
    expect(after.activeJobs).toEqual([1, 2]);
  });

  it("does not destroy caches over a value it simply cannot store", async () => {
    // A full area is a cache to shrink. Any other refusal is a bug in the caller, and
    // answering it by deleting a tab's headers would turn one into two.
    await browser.storage.session.set({ "ledger:1": big(20_000) });
    refuse(Infinity, new Error("Value could not be serialized"));

    expect(await session.store({ "ledger:2": { bad: 1 } })).toBe(false);
    expect((await stored())["ledger:1"]).toBeDefined();
  });

  it("does not reject when even the reclaim fails", async () => {
    refuse(Infinity);
    vi.spyOn(browser.storage.session, "get").mockRejectedValue(new Error("no"));

    await expect(session.store({ "ledger:1": big(5_000) })).resolves.toBe(false);
  });

  it("measures a value the way the area charges for it", () => {
    expect(session.measure({ a: 1 })).toBe(JSON.stringify({ a: 1 }).length);
    expect(session.measure(undefined)).toBe(0);
  });
});
