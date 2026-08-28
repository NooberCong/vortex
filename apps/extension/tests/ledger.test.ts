import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { RequestEnvelope } from "@vortex/proto";
import * as ledger from "@/src/ledger";

const envelope = (url: string, extra: Partial<RequestEnvelope> = {}): RequestEnvelope => ({
  url,
  method: "GET",
  headers: [["Referer", "https://example.com/page"]],
  tabId: 7,
  capturedAt: 1,
  ...extra,
});

describe("the request ledger", () => {
  beforeEach(() => {
    fakeBrowser.reset();
    ledger.__resetForTests();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("gives back the headers the browser used for a URL", async () => {
    ledger.record(envelope("https://cdn.example.com/a.iso"));
    const found = await ledger.lookup(7, "https://cdn.example.com/a.iso");
    expect(found?.headers).toEqual([["Referer", "https://example.com/page"]]);
  });

  it("answers with the newest record when a URL is re-minted", async () => {
    // A signed URL is re-issued under the same path with a fresh signature. Handing the
    // engine the older one is handing it a 403.
    ledger.record(envelope("https://cdn.example.com/a.iso?sig=old", { capturedAt: 1 }));
    ledger.record(envelope("https://cdn.example.com/a.iso?sig=new", { capturedAt: 2 }));
    const found = await ledger.lookup(7, "https://cdn.example.com/a.iso?sig=new");
    expect(found?.capturedAt).toBe(2);
  });

  it("falls back to the path when the query no longer matches", async () => {
    ledger.record(envelope("https://cdn.example.com/a.iso?sig=old"));
    const found = await ledger.lookup(7, "https://cdn.example.com/a.iso?sig=different");
    expect(found?.url).toBe("https://cdn.example.com/a.iso?sig=old");
  });

  it("matches on the post-redirect URL as well as the original", async () => {
    ledger.record(
      envelope("https://example.com/dl", { finalUrl: "https://cdn.example.com/a.iso" }),
    );
    expect((await ledger.lookup(7, "https://cdn.example.com/a.iso"))?.url).toBe(
      "https://example.com/dl",
    );
  });

  it("keeps the newest 500 entries and no more", async () => {
    for (let i = 0; i < 620; i++) ledger.record(envelope(`https://example.com/${i}`));
    await ledger.flush();

    expect(await ledger.lookup(7, "https://example.com/619")).not.toBeNull();
    expect(await ledger.lookup(7, "https://example.com/120")).not.toBeNull();
    // 0..119 have been evicted: 620 - 500 = 120.
    expect(await ledger.lookup(7, "https://example.com/119")).toBeNull();
  });

  it("stops at the byte ceiling long before it reaches the entry ceiling", async () => {
    // The count is not the thing that runs out. A remembered POST body is orders of
    // magnitude bigger than a header list, and the session area is one pool shared with
    // every other tab — so five hundred of these would be the whole extension's budget
    // spent on one tab, and the write would start failing for everything.
    for (let i = 0; i < 12; i++) {
      ledger.record(envelope(`https://example.com/${i}`, { bodyBase64: "x".repeat(64 * 1024) }));
    }
    await ledger.flush();

    const kept = (await fakeBrowser.storage.session.get("ledger:7"))["ledger:7"] as RequestEnvelope[];
    expect(kept.length).toBeLessThan(12);
    expect(kept.reduce((n, e) => n + JSON.stringify(e).length, 0)).toBeLessThanOrEqual(256 * 1024);
    // And the newest is what survived, which is the entry a download starting now is about.
    expect(await ledger.lookup(7, "https://example.com/11")).not.toBeNull();
    expect(await ledger.lookup(7, "https://example.com/0")).toBeNull();
  });

  it("keeps the newest entry even when it is the whole budget by itself", async () => {
    // An oversized entry costs the ones behind it, never itself. A ledger that answered
    // nothing at all would be the more expensive outcome by far.
    ledger.record(envelope("https://example.com/old"));
    ledger.record(
      envelope("https://example.com/huge", { headers: [["X-Big", "h".repeat(300 * 1024)]] }),
    );
    await ledger.flush();

    expect(await ledger.lookup(7, "https://example.com/huge")).not.toBeNull();
    expect(await ledger.lookup(7, "https://example.com/old")).toBeNull();
  });

  it("still answers a lookup when the write it depends on could not happen", async () => {
    // A full session area used to arrive here as an unhandled rejection out of `flush`,
    // which `lookup` awaits — so a tab that ran out of room lost its headers *and* threw
    // at whatever went asking for them. A write that cannot happen is a cache miss.
    vi.spyOn(browser.storage.session, "set").mockRejectedValue(new Error("quota exceeded"));
    ledger.record(envelope("https://example.com/a"));

    await expect(ledger.lookup(7, "https://example.com/a")).resolves.toBeNull();
    await expect(ledger.flush()).resolves.toBeUndefined();
  });

  it("does not lose entries when two flushes overlap", async () => {
    // Storage is read-modify-write and `webRequest` fires faster than a round trip. Two
    // unsynchronised flushes interleave and the second overwrites the first — the classic
    // lost-update, and it presents as a download that occasionally has no headers.
    ledger.record(envelope("https://example.com/one"));
    const first = ledger.flush();
    ledger.record(envelope("https://example.com/two"));
    const second = ledger.flush();
    await Promise.all([first, second]);

    expect(await ledger.lookup(7, "https://example.com/one")).not.toBeNull();
    expect(await ledger.lookup(7, "https://example.com/two")).not.toBeNull();
  });

  it("does not let a concurrent flush hide a record from a lookup", async () => {
    // Two things read the ledger at once — a takeover and a renewal — and both flush
    // first. The second flush finds the buffer already drained and, if it returns without
    // waiting for the first one's write, reads storage that does not have the entry yet.
    // It presents as a handoff that intermittently loses its headers, which is close to
    // the worst kind of bug: rare, invisible, and blamed on the server.
    ledger.record(envelope("https://example.com/late"));
    const [, found] = await Promise.all([
      ledger.flush(),
      ledger.lookup(7, "https://example.com/late"),
    ]);
    expect(found).not.toBeNull();
  });

  it("keeps tabs apart", async () => {
    ledger.record(envelope("https://example.com/a", { tabId: 1 }));
    ledger.record(envelope("https://example.com/b", { tabId: 2 }));
    expect(await ledger.lookup(1, "https://example.com/b")).toBeNull();
    expect(await ledger.lookup(2, "https://example.com/b")).not.toBeNull();
  });

  it("forgets a closed tab, cookies and all", async () => {
    ledger.record(envelope("https://example.com/a", { cookies: "session=secret" }));
    await ledger.flush();
    ledger.forget(7);
    expect(await ledger.lookup(7, "https://example.com/a")).toBeNull();
    // Not merely unreachable — actually gone from storage. A retained cookie header is a
    // retained credential.
    expect(await fakeBrowser.storage.session.get("ledger:7")).toEqual({});
  });

  it("ignores requests with no tab, because there is nothing to file them under", async () => {
    ledger.record(envelope("https://example.com/a", { tabId: undefined }));
    await ledger.flush();
    expect(await ledger.lookup(undefined, "https://example.com/a")).toBeNull();
  });
});
