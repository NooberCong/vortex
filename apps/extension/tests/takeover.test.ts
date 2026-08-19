import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event } from "@vortex/proto";
import * as host from "@/src/host";
import * as ledger from "@/src/ledger";
import * as settings from "@/src/settings";
import * as takeover from "@/src/takeover";

/**
 * Takeover, which is where the extension can do real damage.
 *
 * Every test here is about one of two guarantees:
 *
 * - **Nothing is cancelled unless the handoff will work.** A cancelled download that
 *   Vortex then fails to fetch is a file the user has simply lost, and they will not know
 *   why. Every check that can decline has to decline *before* `downloads.cancel`.
 * - **When it does work, the browser's own headers go with it.** A takeover that replays
 *   a bare GET gets a 403 from every CDN that matters.
 */

type Item = Parameters<Parameters<typeof browser.downloads.onCreated.addListener>[0]>[0];

const ITEM = {
  id: 1,
  url: "https://cdn.example.com/big.iso",
  referrer: "https://example.com/downloads",
  totalBytes: 500 * 1024 * 1024,
  state: "in_progress",
  incognito: false,
} as unknown as Item;

/** A stand-in for `vortex-host`: records commands, and answers the ones we tell it to. */
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

/** `vortexd`'s own defaults, which is what the daemon would answer with on a fresh install. */
const SETTINGS = {
  downloadDir: "C:/Users/x/Downloads",
  categoryDirs: {},
  maxConnections: 16,
  maxConcurrentJobs: 4,
  globalSpeedLimit: 0,
  enableH3: false,
  enableCapture: true,
  siteOptouts: [],
  minCaptureBytes: 1024 * 1024,
  defaultMediaHeight: 1080,
  container: "Auto",
  subtitles: true,
  theme: "System",
  reducedMotion: false,
  clipboardMonitor: true,
  autostart: true,
  writeBufferBudget: 268435456,
} as unknown as Extract<Event, { event: "settingsChanged" }>["settings"];

/** Answers `Ping` and approves every `Probe`. The cooperative daemon. */
const willing = (command: Command): Event | null => {
  if (command.cmd === "ping") return { event: "pong" };
  if (command.cmd === "getSettings") {
    return { event: "settingsChanged", settings: SETTINGS };
  }
  if (command.cmd === "probe") {
    return {
      event: "probed",
      result: {
        url: command.envelope.url,
        finalUrl: command.envelope.url,
        filename: "big.iso",
        host: "cdn.example.com",
        mode: "Parallel",
        resumable: true,
        category: "Other",
      },
    };
  }
  return null;
};

/**
 * `downloads.onCreated` has no in-memory implementation in the fake browser, so the
 * listener is captured on registration and invoked directly. That is the whole of the
 * platform's contribution to this path anyway: it hands over an item and gets out of the
 * way.
 */
let onCreated: ((item: Item) => void) | null = null;

function created(item: Item = ITEM): Promise<void> {
  onCreated?.(item);
  // The listener returns immediately and does its work asynchronously; a macrotask turn
  // is enough for the ping, the probe and the cancel to settle.
  return new Promise((resolve) => setTimeout(resolve, 20));
}

describe("download takeover", () => {
  let cancelled: number[];
  let erased: number[];
  /**
   * What `downloads.search` answers, which the fake browser does not model and the
   * takeover path now depends on. A real download is a moving target: `DownloadItem` is
   * a snapshot from `onCreated` and the file can be finished a moment later, so the tests
   * have to be able to move it.
   */
  let state: string;

  beforeEach(() => {
    fakeBrowser.reset();
    ledger.__resetForTests();
    settings.__resetForTests();
    takeover.__resetForTests();
    host.__resetForTests();

    cancelled = [];
    erased = [];
    state = "in_progress";
    vi.spyOn(browser.downloads, "search").mockImplementation(
      async () => [{ id: ITEM.id, state }] as never,
    );
    vi.spyOn(browser.downloads, "cancel").mockImplementation(async (id: number) => {
      cancelled.push(id);
      // What the platform actually does — and only for a download that was running.
      // Chrome resolves this as a no-op for one that already finished.
      if (state === "in_progress") state = "interrupted";
    });
    vi.spyOn(browser.downloads, "erase").mockImplementation(async ({ id }: { id?: number }) => {
      erased.push(id as number);
      return [] as never;
    });
    vi.spyOn(browser.tabs, "query").mockResolvedValue([{ id: 7 }] as never);
    vi.spyOn(browser.tabs, "get").mockResolvedValue({
      id: 7,
      url: "https://example.com/downloads",
      title: "Downloads — Example",
    } as never);
    vi.spyOn(browser.cookies, "getAll").mockResolvedValue([] as never);

    onCreated = null;
    vi.spyOn(browser.downloads.onCreated, "addListener").mockImplementation((listener) => {
      onCreated = listener as (item: Item) => void;
    });
    // Chrome-only, and absent here: the Firefox path, where the browser's resolved
    // filename is simply not available and the URL has to do.
    Reflect.deleteProperty(browser.downloads, "onDeterminingFilename");

    takeover.watch();
  });

  it("hands the download over, with the headers the browser used", async () => {
    ledger.record({
      url: ITEM.url,
      method: "GET",
      headers: [
        ["User-Agent", "Mozilla/5.0"],
        ["Referer", "https://example.com/downloads"],
      ],
      cookies: "session=abc",
      tabId: 7,
      capturedAt: 1,
    });
    const sent = fakeDaemon(willing);

    await created();

    expect(cancelled).toEqual([1]);
    const submit = sent.find((c) => c.cmd === "submit");
    expect(submit, "the job was never submitted").toBeDefined();
    if (submit?.cmd !== "submit") throw new Error("unreachable");
    expect(submit.spec.envelope.headers).toContainEqual(["User-Agent", "Mozilla/5.0"]);
    expect(submit.spec.envelope.cookies).toBe("session=abc");
    expect(submit.spec.envelope.pageTitle).toBe("Downloads — Example");
  });

  it("does nothing at all when the daemon is unreachable", async () => {
    // The rule the whole module is built around. A download manager that eats the user's
    // download because its own service crashed is worse than no download manager.
    vi.spyOn(browser.runtime, "connectNative").mockImplementation(() => {
      throw new Error("no such native host");
    });

    await created();

    expect(cancelled).toEqual([]);
  });

  it("does nothing when the daemon is there but never answers", async () => {
    // A hung host is not a working host. Reachability is a round trip, not a handle.
    const sent = fakeDaemon(() => null);
    await new Promise<void>((resolve) => {
      void created().then(resolve);
      vi.useFakeTimers();
      void vi.advanceTimersByTimeAsync(3000).then(() => vi.useRealTimers());
    });
    expect(cancelled).toEqual([]);
    expect(sent.some((c) => c.cmd === "submit")).toBe(false);
  });

  it("leaves the browser alone when the probe cannot get bytes", async () => {
    // The probe runs *before* the erase precisely so this case is invisible: the URL was
    // single-use, or fingerprint-gated, and the user just sees an ordinary download.
    const sent = fakeDaemon((command) =>
      command.cmd === "probe"
        ? { event: "error", message: "That link has expired." }
        : willing(command),
    );

    await created();

    expect(cancelled).toEqual([]);
    expect(sent.some((c) => c.cmd === "submit")).toBe(false);
    expect(sent.some((c) => c.cmd === "probe")).toBe(true);
  });

  it("never touches a download from a denylisted origin", async () => {
    const sent = fakeDaemon(willing);
    await created({
      ...ITEM,
      url: "https://ipv4-c001.nflxvideo.net/segment.ism",
    } as Item);

    expect(cancelled).toEqual([]);
    // Not even a ping: the check happens before the daemon is consulted, so a protected
    // origin produces no traffic and no record anywhere.
    expect(sent).toEqual([]);
  });

  it("leaves small files to the browser", async () => {
    const sent = fakeDaemon(willing);
    await created({ ...ITEM, totalBytes: 4096 } as Item);
    expect(cancelled).toEqual([]);
    expect(sent.some((c) => c.cmd === "probe" || c.cmd === "submit")).toBe(false);
  });

  it("takes over a file of unknown size, which is where the big ones hide", async () => {
    // A chunked response has no `Content-Length`. Declining on "no size" would decline
    // exactly the transfers parallel fetching helps most.
    fakeDaemon(willing);
    await created({ ...ITEM, totalBytes: 0, fileSize: 0 } as Item);
    expect(cancelled).toEqual([1]);
  });

  it("ignores anything that is not an http(s) download", async () => {
    const sent = fakeDaemon(willing);
    await created({ ...ITEM, url: "blob:https://example.com/9f3c" } as Item);
    expect(cancelled).toEqual([]);
    // A `blob:` URL is rejected before the daemon is consulted at all.
    expect(sent).toEqual([]);
  });

  it("leaves private-window downloads out of a persistent queue", async () => {
    const sent = fakeDaemon(willing);
    await created({ ...ITEM, incognito: true } as Item);
    expect(cancelled).toEqual([]);
    expect(sent).toEqual([]);
  });

  it("leaves a small file alone when the browser finished it during the probe", async () => {
    // The bug this guards: a 2 MB file is on disk before the probe comes back, and
    // `downloads.cancel` resolves anyway because cancelling a finished download is a
    // no-op rather than an error. Erasing then hides the file the browser wrote and the
    // submit fetches it a second time.
    const sent = fakeDaemon((command) => {
      if (command.cmd === "probe") state = "complete";
      return willing(command);
    });

    await created({ ...ITEM, totalBytes: 0, fileSize: 0 } as Item);

    expect(sent.some((c) => c.cmd === "submit"), "downloaded a second time").toBe(false);
    expect(erased, "erased a download the browser had already written").toEqual([]);
  });

  it("leaves a file alone that was already complete before the probe", async () => {
    // The same race one step earlier: the filename grace alone is longer than a fast
    // small transfer, so the item can be finished before we ask the daemon anything.
    state = "complete";
    const sent = fakeDaemon(willing);

    await created({ ...ITEM, totalBytes: 0, fileSize: 0 } as Item);

    expect(cancelled).toEqual([]);
    expect(erased).toEqual([]);
    // Not even a probe: nothing about a finished download is worth a round trip.
    expect(sent.some((c) => c.cmd === "probe" || c.cmd === "submit")).toBe(false);
  });

  it("does not submit a job for a download that vanished under it", async () => {
    // The user hit cancel, or it completed from cache, while the probe was in flight.
    // Submitting anyway would start a download nobody asked for.
    vi.spyOn(browser.downloads, "cancel").mockRejectedValue(new Error("no such download"));
    const sent = fakeDaemon(willing);

    await created();

    expect(sent.some((c) => c.cmd === "submit")).toBe(false);
  });
});
