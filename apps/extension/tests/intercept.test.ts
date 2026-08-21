import { beforeEach, describe, expect, it, vi } from "vitest";
import { browser } from "wxt/browser";
import { fakeBrowser } from "wxt/testing";

import type { Command, Event } from "@vortex/proto";
import * as capture from "@/src/capture";
import * as host from "@/src/host";
import * as intercept from "@/src/intercept";
import * as settings from "@/src/settings";

/**
 * Pre-emption, which is where the extension can break a page.
 *
 * Channel 2 acts on a download the browser has already decided to make. This channel acts
 * one step earlier, on a response — so it is *predicting* that verdict, and a wrong
 * prediction cancels a request the browser would have rendered. Every test here is about
 * one of two guarantees:
 *
 * - **Nothing is cancelled that is not unambiguously a download.** Not an XHR, not a POST,
 *   not an `inline` disposition, not a redirect.
 * - **Nothing is cancelled unless the daemon answered.** Anything else lets the response
 *   through, where channel 2 sees an ordinary download and gets its own turn.
 */

/** A response the browser is about to turn into a download. */
const ATTACHMENT = {
  requestId: "42",
  url: "https://cdn.example.com/big.iso",
  method: "GET",
  type: "main_frame",
  statusCode: 200,
  tabId: 7,
  originUrl: "https://example.com/downloads",
  responseHeaders: [
    { name: "Content-Type", value: "application/octet-stream" },
    { name: "Content-Length", value: String(500 * 1024 * 1024) },
    { name: "Content-Disposition", value: 'attachment; filename="big.iso"' },
  ],
};

type Response = typeof ATTACHMENT;

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

const SETTINGS = {
  enableCapture: true,
  siteOptouts: [],
  minCaptureBytes: 1024 * 1024,
  defaultMediaHeight: 1080,
  subtitles: true,
} as unknown as Extract<Event, { event: "settingsChanged" }>["settings"];

/** Answers `GetSettings` and approves every `Probe`. The cooperative daemon. */
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

/** An event listener, invoked with whatever the platform would have handed it. */
type Fired = (details: Record<string, unknown>) => void;

/**
 * Captures a listener at registration instead of implementing the event.
 *
 * The platform's whole contribution to these paths is to call a listener with an object,
 * and the fake browser implements none of `webRequest`, so the tests do that part
 * themselves. The cast is the price of one helper standing in for listener signatures that
 * have nothing in common but their name.
 */
function record(event: { addListener: (...args: never[]) => void }, into: (l: Fired) => void) {
  vi.spyOn(event, "addListener").mockImplementation(((listener: Fired) => into(listener)) as never);
}

/** The blocking listener, captured at registration the way the browser would call it. */
let onHeadersReceived: ((details: Response) => unknown) | null = null;

/** What the listener told the browser to do: `{cancel:true}`, `{}`, or nothing at all. */
async function respond(details: Response = ATTACHMENT): Promise<{ cancel?: boolean } | undefined> {
  const answer = onHeadersReceived?.(details);
  return (await answer) as { cancel?: boolean } | undefined;
}

describe("response pre-emption", () => {
  beforeEach(() => {
    fakeBrowser.reset();
    capture.__resetForTests();
    settings.__resetForTests();
    host.__resetForTests();

    vi.spyOn(browser.tabs, "get").mockResolvedValue({
      id: 7,
      url: "https://example.com/downloads",
      title: "Downloads — Example",
    } as never);
    vi.spyOn(browser.cookies, "getAll").mockResolvedValue([] as never);

    onHeadersReceived = null;
    vi.spyOn(browser.webRequest.onHeadersReceived, "addListener").mockImplementation(
      (listener) => {
        onHeadersReceived = listener as unknown as (details: Response) => unknown;
      },
    );

    intercept.watch();
  });

  it("registers as a blocking listener, which is the whole point of it", () => {
    expect(browser.webRequest.onHeadersReceived.addListener).toHaveBeenCalledWith(
      expect.any(Function),
      { urls: ["<all_urls>"] },
      ["blocking", "responseHeaders"],
    );
  });

  it("cancels the response and submits the job, before any download exists", async () => {
    const sent = fakeDaemon(willing);

    const answer = await respond();

    expect(answer).toEqual({ cancel: true });
    const submit = sent.find((c) => c.cmd === "submit");
    expect(submit, "the job was never submitted").toBeDefined();
    if (submit?.cmd !== "submit") throw new Error("unreachable");
    // Nothing here came from a `DownloadItem`, because there never was one.
    expect(submit.spec.envelope.url).toBe(ATTACHMENT.url);
    expect(submit.spec.envelope.filenameHint).toBe("big.iso");
    expect(submit.spec.envelope.contentLength).toBe(500 * 1024 * 1024);
    expect(submit.spec.envelope.headers).toContainEqual([
      "Referer",
      "https://example.com/downloads",
    ]);
    // The page the user clicked from, not the file's own URL — that is what names a file
    // whose URL is a hash.
    expect(submit.spec.envelope.pageUrl).toBe("https://example.com/downloads");
    expect(submit.spec.envelope.pageTitle).toBe("Downloads — Example");
    // No browser-resolved filename exists at this point, so none is claimed.
    expect(submit.spec.filename ?? null).toBeNull();
  });

  it("replays the headers the browser actually sent, when the observer has them", async () => {
    // The one advantage this channel has over channel 2: `requestId` identifies the
    // request exactly, so there is no matching a download back to a request by URL.
    //
    // The fake browser has no in-memory `webRequest`, so the observer's own listeners are
    // captured on registration and driven directly — which is all the platform does here
    // anyway. Its `onHeadersReceived` is swallowed rather than kept: the one this test
    // drives is the blocking listener captured in `beforeEach`.
    let began: Fired = () => {};
    let sending: Fired = () => {};
    record(browser.webRequest.onBeforeRequest, (listener) => (began = listener));
    record(browser.webRequest.onSendHeaders, (listener) => (sending = listener));
    for (const event of [
      browser.webRequest.onHeadersReceived,
      browser.webRequest.onCompleted,
      browser.webRequest.onErrorOccurred,
    ]) {
      record(event, () => {});
    }
    record(browser.tabs.onRemoved, () => {});
    capture.observe(() => {});

    began({
      requestId: ATTACHMENT.requestId,
      url: ATTACHMENT.url,
      method: "GET",
      type: "main_frame",
      tabId: 7,
      timeStamp: 1,
    });
    sending({
      requestId: ATTACHMENT.requestId,
      url: ATTACHMENT.url,
      requestHeaders: [
        { name: "User-Agent", value: "Mozilla/5.0" },
        { name: "Cookie", value: "session=abc" },
      ],
    });
    const sent = fakeDaemon(willing);

    await respond();

    const submit = sent.find((c) => c.cmd === "submit");
    if (submit?.cmd !== "submit") throw new Error("the job was never submitted");
    expect(submit.spec.envelope.headers).toContainEqual(["User-Agent", "Mozilla/5.0"]);
    expect(submit.spec.envelope.cookies).toBe("session=abc");
  });

  it("lets the response through when the probe cannot get bytes", async () => {
    // The URL was single-use, or fingerprint-gated. The user gets an ordinary download and
    // never learns Vortex considered it.
    const sent = fakeDaemon((command) =>
      command.cmd === "probe"
        ? { event: "error", message: "That link has expired." }
        : willing(command),
    );

    expect(await respond()).toEqual({});
    expect(sent.some((c) => c.cmd === "submit")).toBe(false);
  });

  it("lets the response through when the daemon is not there", async () => {
    // The rule the whole capture layer is built around, and the case that matters most
    // here: a suspended response with nobody coming for it is a page that never loads.
    vi.spyOn(browser.runtime, "connectNative").mockImplementation(() => {
      throw new Error("no such native host");
    });

    expect(await respond()).toEqual({});
  });

  it("gives up on a daemon that never answers, rather than holding the response", async () => {
    vi.useFakeTimers();
    try {
      fakeDaemon(() => null);
      const answer = respond();
      await vi.advanceTimersByTimeAsync(5000);
      expect(await answer).toEqual({});
    } finally {
      vi.useRealTimers();
    }
  });

  it("ignores anything that is not a navigation", async () => {
    // A page that fetches an attachment endpoint and builds a blob from it is not
    // downloading anything. Cancelling that is a broken site.
    const sent = fakeDaemon(willing);
    expect(await respond({ ...ATTACHMENT, type: "xmlhttprequest" })).toBeUndefined();
    expect(sent).toEqual([]);
  });

  it("ignores a POST, which is not safe to send twice", async () => {
    const sent = fakeDaemon(willing);
    expect(await respond({ ...ATTACHMENT, method: "POST" })).toBeUndefined();
    expect(sent).toEqual([]);
  });

  it("ignores a redirect and an error, neither of which is a file yet", async () => {
    fakeDaemon(willing);
    expect(await respond({ ...ATTACHMENT, statusCode: 302 })).toBeUndefined();
    expect(await respond({ ...ATTACHMENT, statusCode: 404 })).toBeUndefined();
  });

  it("does not mistake an inline disposition for an attachment", async () => {
    // `inline; filename="attachment.pdf"` contains the word and means the opposite.
    const sent = fakeDaemon(willing);
    const answer = await respond({
      ...ATTACHMENT,
      responseHeaders: [{ name: "Content-Disposition", value: 'inline; filename="report.pdf"' }],
    });
    expect(answer).toBeUndefined();
    expect(sent).toEqual([]);
  });

  it("leaves a response with no disposition to the browser's own verdict", async () => {
    // An `application/octet-stream` navigation usually becomes a download and sometimes
    // does not, and only the browser knows which. Channel 2 acts on the answer instead of
    // guessing at it.
    const sent = fakeDaemon(willing);
    const answer = await respond({
      ...ATTACHMENT,
      responseHeaders: [{ name: "Content-Type", value: "application/octet-stream" }],
    });
    expect(answer).toBeUndefined();
    expect(sent).toEqual([]);
  });

  it("never touches a response from a denylisted origin", async () => {
    const sent = fakeDaemon(willing);
    const answer = await respond({ ...ATTACHMENT, url: "https://ipv4-c001.nflxvideo.net/f.ism" });
    expect(answer).toEqual({});
    // Not even a probe: the check happens before the daemon is consulted at all.
    expect(sent).toEqual([]);
  });

  it("leaves small files to the browser", async () => {
    const sent = fakeDaemon(willing);
    const answer = await respond({
      ...ATTACHMENT,
      responseHeaders: [
        { name: "Content-Length", value: "4096" },
        { name: "Content-Disposition", value: "attachment" },
      ],
    });
    expect(answer).toEqual({});
    expect(sent.some((c) => c.cmd === "probe" || c.cmd === "submit")).toBe(false);
  });

  it("takes a file of unknown size, which is where the big ones hide", async () => {
    const sent = fakeDaemon(willing);
    const answer = await respond({
      ...ATTACHMENT,
      responseHeaders: [{ name: "Content-Disposition", value: "attachment" }],
    });
    expect(answer).toEqual({ cancel: true });
    expect(sent.some((c) => c.cmd === "submit")).toBe(true);
  });

  it("keeps a private-window download out of a persistent queue", async () => {
    const sent = fakeDaemon(willing);
    expect(await respond({ ...ATTACHMENT, incognito: true } as Response)).toEqual({});
    expect(sent).toEqual([]);
  });

  /**
   * The same receipt channel 2 owes, for the same reason and in the same words.
   *
   * This channel's advantage is that nothing is ever written and nothing appears in the
   * browser's download list — which is also exactly what makes it indistinguishable, from
   * the page, from a link that did nothing. The two channels must not differ in what they
   * say about that, which is why `hand` is shared (`src/handoff.ts`).
   */
  describe("the receipt", () => {
    let told: Array<[number, unknown]>;

    beforeEach(() => {
      told = [];
      vi.spyOn(browser.tabs, "sendMessage").mockImplementation(
        async (tabId: number, message: unknown) => {
          told.push([tabId, message]);
          return undefined as never;
        },
      );
    });

    it("names the file from the disposition, in the tab that asked for it", async () => {
      fakeDaemon(willing);
      await respond();
      expect(told).toEqual([[7, { kind: "captured", filename: "big.iso" }]]);
    });

    it("says nothing about a response it let through", async () => {
      fakeDaemon((command) =>
        command.cmd === "probe"
          ? { event: "error", message: "That link has expired." }
          : willing(command),
      );
      expect(await respond()).toEqual({});
      expect(told).toEqual([]);
    });

    it("has nowhere to draw for a response that belongs to no tab", async () => {
      // `tabId` is -1 for a request the browser did not make on a tab's behalf. The badge
      // is what covers that case, and it covers it whatever the page situation is.
      fakeDaemon(willing);
      await respond({ ...ATTACHMENT, tabId: -1 });
      expect(told).toEqual([]);
    });
  });
});
