import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event, MediaCandidate } from "@vortex/proto";
import * as host from "@/src/host";
import * as media from "@/src/media";
import { RETRY_AFTER } from "@/src/media";
import * as settings from "@/src/settings";

/**
 * The probe de-duplicator (03 §3).
 *
 * One manifest URL is seen dozens of times — a live-ish playlist is re-fetched every few
 * seconds, and a player that re-mints its URL produces a new one on every renewal. So the
 * sniffer remembers what it has asked about. The question this file exists for is what
 * "asked about" should mean when the asking went nowhere: a probe can fail for reasons
 * that say nothing about the URL, and a URL retired on that basis is a permanent silent
 * failure produced by a transient one. There is no error path back from `ProbeMedia` —
 * the daemon answers with a ladder or it does not answer at all — so the only thing that
 * can distinguish the two is time.
 */

const MANIFEST = "https://cdn.example.com/hls/master.m3u8";
const PAGE = "https://example.com/watch/1";
const TAB = 7;

const SETTINGS = {
  enableCapture: true,
  siteOptouts: [],
  minCaptureBytes: 1024 * 1024,
  defaultMediaHeight: 1080,
  subtitles: true,
} as unknown as Extract<Event, { event: "settingsChanged" }>["settings"];

/** A stand-in for `vortex-host`: records commands, answers the ones we tell it to. */
function fakeDaemon(reply: (command: Command) => Event | null) {
  const sent: Command[] = [];
  const listeners: Array<(event: unknown) => void> = [];
  const port = {
    postMessage(command: Command) {
      sent.push(command);
      const event = reply(command);
      if (event) queueMicrotask(() => listeners.forEach((l) => l(event)));
    },
    disconnect() {},
    onMessage: { addListener: (l: (event: unknown) => void) => listeners.push(l) },
    onDisconnect: { addListener: () => {} },
  };
  vi.spyOn(browser.runtime, "connectNative").mockReturnValue(port as never);
  return sent;
}

const willing = (command: Command): Event | null =>
  command.cmd === "ping" ? { event: "pong" } : null;

/** A daemon that answers a probe with a one-rung ladder, as a reachable manifest would. */
const answering = (command: Command): Event | null => {
  if (command.cmd === "ping") return { event: "pong" };
  if (command.cmd !== "probeMedia") return null;
  const candidate = {
    id: "a",
    manifestUrl: MANIFEST,
    kind: "hls",
    title: "Example",
    durationSecs: 634,
    live: false,
    variants: [{ id: "v1", height: 1080 }],
    audio: [],
    subtitles: [],
  } as unknown as MediaCandidate;
  return { event: "mediaFound", tab: TAB, candidates: [candidate] } as Event;
};

const probes = (sent: Command[]) => sent.filter((command) => command.cmd === "probeMedia");

/** One manifest going past on the wire, as channel 1 would report it. */
function seen(): void {
  media.inspect({
    url: MANIFEST,
    tabId: TAB,
    type: "xmlhttprequest",
    observed: { mimeType: "application/x-mpegurl" },
  });
}

/**
 * Lets the fire-and-forget probe chain finish.
 *
 * `inspect` is called from a `webRequest` listener, which cannot await anything, so the
 * probe is several promises deep by the time a command reaches the port: the settings, the
 * tab, the ledger lookup. None of them is a timer, so turning the microtask queue over is
 * enough and the fake clock stays exactly where the test put it.
 */
async function settle(): Promise<void> {
  for (let i = 0; i < 30; i++) await Promise.resolve();
}

describe("asking the daemon about the same manifest twice", () => {
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
    vi.useFakeTimers();
    media.listen();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("asks once while the answer is still outstanding", async () => {
    // The ordinary case, and the reason the memory exists at all: a player re-fetching its
    // playlist every few seconds must not become a probe every few seconds.
    const sent = fakeDaemon(willing);
    for (let i = 0; i < 5; i++) {
      seen();
      await settle();
      vi.advanceTimersByTime(2000);
    }
    expect(probes(sent)).toHaveLength(1);
  });

  it("asks again about a manifest whose probe produced nothing", async () => {
    // The daemon was restarting, or the origin refused the replayed envelope. Neither is a
    // fact about the URL, and the player is still fetching it. Retiring it here is a
    // permanent failure manufactured out of a transient one.
    const sent = fakeDaemon(willing);
    seen();
    await settle();
    expect(probes(sent)).toHaveLength(1);

    vi.advanceTimersByTime(RETRY_AFTER + 1);
    seen();
    await settle();
    expect(probes(sent)).toHaveLength(2);
  });

  it("never asks again about a manifest that produced a ladder", async () => {
    // The other half of the same rule. An answered manifest is answered, and re-probing it
    // every thirty seconds for the life of the worker would be the bug this fix invented.
    const sent = fakeDaemon(answering);
    seen();
    await settle();
    expect(probes(sent)).toHaveLength(1);

    vi.advanceTimersByTime(RETRY_AFTER * 10);
    seen();
    await settle();
    expect(probes(sent)).toHaveLength(1);
  });
});
