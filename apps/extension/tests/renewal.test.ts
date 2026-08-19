import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event } from "@vortex/proto";
import * as host from "@/src/host";
import * as ledger from "@/src/ledger";
import * as renewal from "@/src/renewal";

/**
 * URL renewal — the resilience feature that only exists if both halves do (03 §Handoff 3).
 *
 * A signed CDN URL expires thirty seconds after it is minted. The engine pauses on its
 * existing bitmap and asks; the extension re-acquires in page context and answers. What
 * these tests pin down is the failure direction: when the page cannot mint a fresh URL,
 * nothing is sent, and the job stays paused and resumable rather than being handed a URL
 * that will fail again.
 */

let deliver: (event: Event) => void;

function fakeDaemon(): Command[] {
  const sent: Command[] = [];
  const listeners: Array<(event: unknown) => void> = [];
  deliver = (event) => listeners.forEach((l) => l(event));
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue({
    postMessage: (command: Command) => sent.push(command),
    disconnect() {},
    onMessage: { addListener: (l: (event: unknown) => void) => listeners.push(l) },
    onDisconnect: { addListener: () => {} },
  } as never);
  // Events only reach the extension down an open port. In the background one is opened
  // at startup; here a ping is the cheapest way to be in the same state.
  host.send({ cmd: "ping" });
  return sent;
}

const EXPIRED: Event = {
  event: "urlExpired",
  job: 42,
  hint: {
    url: "https://cdn.example.com/a.iso?sig=stale",
    pageUrl: "https://example.com/downloads",
    tabId: 7,
    reason: "403 on a range that was already reading",
  },
};

const settle = () => new Promise((resolve) => setTimeout(resolve, 20));

describe("renewing an expired URL", () => {
  beforeEach(() => {
    fakeBrowser.reset();
    ledger.__resetForTests();
    host.__resetForTests();
    vi.spyOn(browser.tabs, "get").mockResolvedValue({
      id: 7,
      url: "https://example.com/downloads",
    } as never);
    vi.spyOn(browser.cookies, "getAll").mockResolvedValue([] as never);
    renewal.listen();
  });

  it("asks the page, and hands the engine what the page came back with", async () => {
    const sent = fakeDaemon();
    vi.spyOn(browser.tabs, "sendMessage").mockResolvedValue(
      "https://cdn.example.com/a.iso?sig=fresh" as never,
    );

    deliver(EXPIRED);
    await settle();

    const renewed = sent.find((c) => c.cmd === "renewedUrl");
    expect(renewed, "the engine was never given a new URL").toBeDefined();
    if (renewed?.cmd !== "renewedUrl") throw new Error("unreachable");
    expect(renewed.job).toBe(42);
    expect(renewed.envelope.url).toBe("https://cdn.example.com/a.iso?sig=fresh");
    // The stale URL must not survive as `finalUrl`, or the engine would resume on it.
    expect(renewed.envelope.finalUrl).toBeUndefined();
  });

  it("prefers the envelope the re-request left in the ledger", async () => {
    // The page's `fetch` goes through `webRequest` like any other, so by the time it
    // resolves the ledger holds real headers for the new URL — captured, not
    // reconstructed. Renewal costs one fetch and yields a better envelope than the
    // original takeover had.
    const sent = fakeDaemon();
    ledger.record({
      url: "https://cdn.example.com/a.iso?sig=fresh",
      method: "GET",
      headers: [["User-Agent", "Mozilla/5.0"]],
      cookies: "session=abc",
      tabId: 7,
      capturedAt: 2,
    });
    vi.spyOn(browser.tabs, "sendMessage").mockResolvedValue(
      "https://cdn.example.com/a.iso?sig=fresh" as never,
    );

    deliver(EXPIRED);
    await settle();

    const renewed = sent.find((c) => c.cmd === "renewedUrl");
    if (renewed?.cmd !== "renewedUrl") throw new Error("the renewal never happened");
    expect(renewed.envelope.headers).toContainEqual(["User-Agent", "Mozilla/5.0"]);
    expect(renewed.envelope.cookies).toBe("session=abc");
  });

  it("says nothing when the page cannot mint a fresh URL", async () => {
    // Giving up gracefully is the specified behaviour: the job stays paused with a
    // "reopen the page" action. Answering with the URL we already know is dead would
    // start a retry loop against a 403.
    const sent = fakeDaemon();
    vi.spyOn(browser.tabs, "sendMessage").mockResolvedValue(null as never);

    deliver(EXPIRED);
    await settle();

    expect(sent.some((c) => c.cmd === "renewedUrl")).toBe(false);
  });

  it("says nothing when the tab is gone", async () => {
    const sent = fakeDaemon();
    vi.spyOn(browser.tabs, "get").mockRejectedValue(new Error("no such tab"));
    vi.spyOn(browser.tabs, "query").mockResolvedValue([] as never);

    deliver(EXPIRED);
    await settle();

    expect(sent.some((c) => c.cmd === "renewedUrl")).toBe(false);
  });

  it("finds the page again after a session restore renumbered the tabs", async () => {
    // A browser restart renumbers every tab it restores, so the hint's id is stale while
    // the page it names is still open. Losing the renewal to that would mean a download
    // that survived the restart still failing.
    const sent = fakeDaemon();
    vi.spyOn(browser.tabs, "get").mockRejectedValue(new Error("no such tab"));
    vi.spyOn(browser.tabs, "query").mockResolvedValue([{ id: 19 }] as never);
    const send = vi
      .spyOn(browser.tabs, "sendMessage")
      .mockResolvedValue("https://cdn.example.com/a.iso?sig=fresh" as never);

    deliver(EXPIRED);
    await settle();

    expect(send).toHaveBeenCalledWith(19, {
      kind: "renew",
      url: "https://cdn.example.com/a.iso?sig=stale",
    });
    expect(sent.some((c) => c.cmd === "renewedUrl")).toBe(true);
  });
});
