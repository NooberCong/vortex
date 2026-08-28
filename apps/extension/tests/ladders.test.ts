import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event, MediaCandidate } from "@vortex/proto";
import * as host from "@/src/host";
import * as media from "@/src/media";
import * as settings from "@/src/settings";

/**
 * What a tab remembers about its videos (03 §3).
 *
 * A ladder is kept per tab so that an overlay re-injected after a worker eviction can ask
 * for it again rather than wait for the manifest to go past a second time. The list is
 * cleared by a navigation and by the tab closing — which is enough for a page that plays
 * one video and nothing at all for the page that never navigates: a player re-mints its
 * manifest URL as the signature expires, and every re-mint is a URL this module has never
 * seen before and appends. Left alone that grows for as long as the tab is open, in a
 * session area shared with every other tab (`src/session.ts`).
 */

const PAGE = "https://example.com/watch/1";
const TAB = 7;

const SETTINGS = {
  enableCapture: true,
  siteOptouts: [],
  minCaptureBytes: 1024 * 1024,
  defaultMediaHeight: 1080,
  subtitles: true,
} as unknown as Extract<Event, { event: "settingsChanged" }>["settings"];

const ladder = (manifestUrl: string, title: string) =>
  ({
    id: manifestUrl,
    manifestUrl,
    kind: "hls",
    title,
    durationSecs: 634,
    live: true,
    variants: [{ id: "v1", height: 1080 }],
    audio: [],
    subtitles: [],
  }) as unknown as MediaCandidate;

/** A daemon that answers every probe with a one-rung ladder for the URL it was asked about. */
function fakeDaemon(): void {
  let answered = 0;
  const listeners: Array<(event: unknown) => void> = [];
  const port = {
    postMessage(command: Command) {
      const event =
        command.cmd === "ping"
          ? ({ event: "pong" } as Event)
          : command.cmd === "probeMedia"
            ? ({
                event: "mediaFound",
                tab: TAB,
                candidates: [ladder(command.envelope.url, `Take ${++answered}`)],
              } as Event)
            : null;
      if (event) queueMicrotask(() => listeners.forEach((l) => l(event)));
    },
    disconnect() {},
    onMessage: { addListener: (l: (event: unknown) => void) => listeners.push(l) },
    onDisconnect: { addListener: () => {} },
  };
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue(port as never);
}

/** One manifest going past on the wire, as channel 1 would report it. */
function seen(url: string): void {
  media.inspect({
    url,
    tabId: TAB,
    type: "xmlhttprequest",
    observed: { mimeType: "application/x-mpegurl" },
  });
}

/** Lets the fire-and-forget probe → answer → store chain finish. See `probe-cache.test.ts`. */
async function settle(): Promise<void> {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}

/** A signed manifest as a live player re-mints it: same path, a fresh signature each time. */
const remint = (n: number) => `https://cdn.example.com/hls/master.m3u8?sig=${n}`;

describe("the ladders a tab holds on to", () => {
  beforeEach(() => {
    fakeBrowser.reset();
    media.__resetForTests();
    settings.__resetForTests();
    host.__resetForTests();
    settings.adopt(SETTINGS);
    vi.spyOn(browser.tabs, "get").mockResolvedValue({
      id: TAB,
      url: PAGE,
      title: "Example",
    } as never);
    fakeDaemon();
    media.listen();
  });

  it("remembers what the daemon found, for an overlay that has to ask again", async () => {
    seen(remint(1));
    await settle();
    expect(await media.known(TAB)).toMatchObject([{ manifestUrl: remint(1), title: "Take 1" }]);
  });

  it("stops growing once a live stream has re-signed its manifest enough times", async () => {
    for (let i = 0; i < 40; i++) {
      seen(remint(i));
      await settle();
    }

    const kept = await media.known(TAB);
    expect(kept).toHaveLength(24);
    // Newest kept: the current signature is the only one that would still play.
    expect(kept.at(-1)?.manifestUrl).toBe(remint(39));
    expect(kept.some((c) => c.manifestUrl === remint(0))).toBe(false);
  });

  it("does not lose a ladder to another one answered in the same turn", async () => {
    // Two manifests go past together — a page with a trailer above the feature, a player
    // that fetches video and audio manifests side by side — and both probes come back
    // before either has been stored. Unserialised, the second read never sees the first
    // ladder and writes a list without it.
    seen(remint(1));
    seen(remint(2));
    await settle();

    expect(await media.known(TAB)).toHaveLength(2);
  });

  it("keeps videos apart when a site identifies them by query", async () => {
    // The tighter fix for the re-minting above is to merge on the path and drop the query.
    // It is also wrong: `/playlist?v=…` is a real shape, and collapsing those would leave a
    // page of videos offering one.
    seen("https://example.com/playlist?v=first");
    await settle();
    seen("https://example.com/playlist?v=second");
    await settle();

    expect(await media.known(TAB)).toHaveLength(2);
  });

  it("updates a ladder in place rather than appending it twice", async () => {
    seen(remint(1));
    await settle();
    // A fresh worker: the probe memory is gone, the tab's ladders are not.
    media.__resetForTests();
    seen(remint(1));
    await settle();

    expect(await media.known(TAB)).toMatchObject([{ manifestUrl: remint(1), title: "Take 2" }]);
  });

  it("forgets everything the moment the tab goes somewhere else", async () => {
    seen(remint(1));
    await settle();
    await fakeBrowser.tabs.onUpdated.trigger(TAB, { status: "loading", url: PAGE }, {} as never);
    await settle();

    expect(await media.known(TAB)).toEqual([]);
  });

  it("forgets everything when the tab closes", async () => {
    seen(remint(1));
    await settle();
    await fakeBrowser.tabs.onRemoved.trigger(TAB, { windowId: 1, isWindowClosing: false });
    await settle();

    expect(await media.known(TAB)).toEqual([]);
  });
});
