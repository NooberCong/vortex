import { beforeEach, describe, expect, it } from "vitest";
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
