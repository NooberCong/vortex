import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event } from "@vortex/proto";
import * as host from "@/src/host";
import * as media from "@/src/media";
import * as settings from "@/src/settings";

/**
 * Channel 5's guards (03 §5).
 *
 * The page asks; this is the half that decides. Every test here is about *not* asking the
 * daemon, because the answer to an unwanted question is an extractor subprocess run against
 * a URL the user did not offer — and the same denylist, opt-out and capture switches that
 * govern every other channel have to govern this one too.
 */

const PAGE = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";

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

const SETTINGS = {
  enableCapture: true,
  siteOptouts: [],
  minCaptureBytes: 1024 * 1024,
  defaultMediaHeight: 1080,
  subtitles: true,
} as unknown as Extract<Event, { event: "settingsChanged" }>["settings"];

const willing = (command: Command): Event | null =>
  command.cmd === "ping" ? { event: "pong" } : null;

/** The probes that actually left the extension. */
const probes = (sent: Command[]) => sent.filter((command) => command.cmd === "probeMedia");

describe("offering a page to the extractor", () => {
  beforeEach(() => {
    fakeBrowser.reset();
    media.__resetForTests();
    settings.__resetForTests();
    // Without this the module keeps the previous test's port, and every command lands in
    // the previous test's recorder.
    host.__resetForTests();
    settings.adopt(SETTINGS);
    vi.spyOn(browser.tabs, "get").mockResolvedValue({
      id: 7,
      url: PAGE,
      title: "Big Buck Bunny",
    } as never);
  });

  it("asks the daemon about the page itself", async () => {
    const sent = fakeDaemon(willing);
    await media.probePage(PAGE, 7);

    const asked = probes(sent);
    expect(asked).toHaveLength(1);
    expect(asked[0]).toMatchObject({ cmd: "probeMedia", envelope: { url: PAGE, tabId: 7 } });
  });

  it("says nothing when a ladder is already known for the tab", async () => {
    // The content script's view can be seconds old. An extraction is a subprocess, and
    // one started for a page that already has a badge on it is pure waste.
    await browser.storage.session.set({
      "media:7": [{ manifestUrl: "https://x/master.m3u8" }],
    });
    const sent = fakeDaemon(willing);
    await media.probePage(PAGE, 7);
    expect(probes(sent)).toHaveLength(0);
  });

  it("says nothing about a page the daemon could not fetch", async () => {
    const sent = fakeDaemon(willing);
    for (const url of ["file:///C:/movies/holiday.html", "about:blank", "chrome://history"]) {
      await media.probePage(url, 7);
    }
    expect(probes(sent)).toHaveLength(0);
  });

  it("says nothing for a request with no tab behind it", async () => {
    const sent = fakeDaemon(willing);
    await media.probePage(PAGE, -1);
    expect(probes(sent)).toHaveLength(0);
  });

  it("says nothing when capture is switched off", async () => {
    settings.adopt({ ...SETTINGS, enableCapture: false });
    const sent = fakeDaemon(willing);
    await media.probePage(PAGE, 7);
    expect(probes(sent)).toHaveLength(0);
  });

  it("says nothing on an origin the user has opted out of", async () => {
    settings.adopt({ ...SETTINGS, siteOptouts: ["https://www.youtube.com"] });
    const sent = fakeDaemon(willing);
    await media.probePage(PAGE, 7);
    expect(probes(sent)).toHaveLength(0);
  });

  it("says nothing about a denylisted site", async () => {
    // The DRM list is a guarantee, not a preference, and channel 5 is the one channel
    // that could hand an extractor a page URL for a site nothing was ever sniffed on.
    const denied = "https://www.netflix.com/watch/80100172";
    vi.spyOn(browser.tabs, "get").mockResolvedValue({ id: 7, url: denied } as never);
    const sent = fakeDaemon(willing);
    await media.probePage(denied, 7);
    expect(probes(sent)).toHaveLength(0);
  });

  it("asks once, however many times the page asks", async () => {
    const sent = fakeDaemon(willing);
    await media.probePage(PAGE, 7);
    await media.probePage(PAGE, 7);
    await media.probePage(PAGE, 7);
    expect(probes(sent)).toHaveLength(1);
  });
});
