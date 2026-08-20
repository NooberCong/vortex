import { browser } from "wxt/browser";

import { MEDIA_CAPTURE, RELEASES } from "@/src/build";
import * as capture from "@/src/capture";
import { describeTab } from "@/src/envelope";
import * as hook from "@/src/hook";
import * as host from "@/src/host";
import * as intercept from "@/src/intercept";
import * as media from "@/src/media";
import type { Internal, PopupState } from "@/src/messages";
import * as renewal from "@/src/renewal";
import * as settings from "@/src/settings";
import * as takeover from "@/src/takeover";

/**
 * The background: a sensor and a remote control, never a downloader.
 *
 * Everything in here obeys the MV3 service worker's rules, because breaking one of them
 * produces a bug that only appears after thirty seconds of idleness and is therefore
 * almost impossible to find in development (03 §Service worker lifetime):
 *
 * - **Every listener is registered synchronously, from this top-level call.** A listener
 *   added inside an `await` is silently lost on the next wake, and the extension appears
 *   to work perfectly until the worker is evicted once.
 * - **Nothing durable is kept in a module variable.** The ledger, the settings and the
 *   per-tab ladders live in `storage.session`; what is held in memory is either in-flight
 *   or reconstructible.
 * - **The alarm is a liveness floor, not a keepalive.** It exists so a port that died
 *   while the worker slept is re-established on the next minute rather than on the next
 *   download. It deliberately does not try to keep the worker alive.
 */
export default defineBackground(() => {
  // ── Channel 1: observe every request, and remember the ones that could matter ──
  capture.observe((details) => {
    if (MEDIA_CAPTURE) media.inspect(details);
  });

  // ── Channel 2: the browser decided something is a file ────────────────────────
  // Firefox first, where the response can be taken before a download exists at all;
  // anything it declines arrives at `takeover` as an ordinary download a moment later.
  intercept.watch();
  takeover.watch();

  // ── Channels 3 and 4, and the overlay they feed ───────────────────────────────
  if (MEDIA_CAPTURE) {
    media.listen();
    browser.runtime.onMessage.addListener((message, sender, respond) => {
      // `true` keeps the channel open for an async reply; anything else closes it, so
      // the branches that answer nothing must not return a promise.
      return answer(message as Internal, sender, respond);
    });
  }

  // ── Renewal: the resilience feature that needs both halves to exist ───────────
  renewal.listen();

  // ── Settings, and the daemon's own announcements of them ──────────────────────
  host.onEvent((event) => {
    if (event.event === "settingsChanged") settings.adopt(event.settings);
  });

  browser.alarms.create("liveness", { periodInMinutes: 1 });
  browser.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name !== "liveness") return;
    // A `Ping` that comes back costs nothing; one that does not drops the dead port so
    // the next real command reconnects instead of failing.
    void host.reachable().then((up) => {
      if (!up) host.reset();
    });
  });

  // The extension is half of Vortex, and it is installed by itself — from a store, from
  // a zip — so the first thing to establish is whether the other half is here at all.
  browser.runtime.onInstalled.addListener((details) => {
    if (details.reason !== "install") return;
    void welcome();
  });

  browser.runtime.onStartup.addListener(() => {
    void settings.refresh();
    if (MEDIA_CAPTURE) void hook.restore();
  });

  // The first wake of a session is not `onStartup` — an install, an update, or an
  // eviction all land here instead, and each of them needs the settings just as much.
  void settings.refresh();
});

/**
 * Opens the download page on a fresh install, if there is anything to install.
 *
 * Only on `missing` — a native host manifest that is not there, which is written during
 * the app's own install and so is the one signal that means the app was never installed
 * on this machine (`src/host.ts`). A `stopped` daemon is an app the user already has, and
 * a tab telling them to go and download it again would be both wrong and the sort of thing
 * that gets an extension removed on day one. So the common orders are both quiet: install
 * the app first and the extension opens nothing, install the extension first and it says
 * where the rest is.
 */
async function welcome(): Promise<void> {
  if ((await host.status()) !== "missing") return;
  await browser.tabs.create({ url: RELEASES });
}

/**
 * Handles a message from a content script.
 *
 * Returns `true` only for the branches that will call `respond` later; returning it
 * unconditionally leaves every message channel open until it times out.
 */
function answer(
  message: Internal,
  sender: { tab?: { id?: number; title?: string; url?: string } },
  respond: (value: unknown) => void,
): boolean {
  const tabId = sender.tab?.id;

  switch (message.kind) {
    case "ready": {
      if (tabId === undefined) return false;
      void media.known(tabId).then((candidates) => {
        respond(candidates);
      });
      return true;
    }

    case "download": {
      // A content script is placed by the browser; the popup has to say where it is. The
      // browser's answer wins wherever there is one, so a page cannot name someone
      // else's tab.
      const on = tabId ?? message.tabId;
      if (on === undefined) return false;
      void submit(message, on, sender.tab);
      return false;
    }

    case "siteCapture": {
      void setCapture(message.origin, message.on);
      return false;
    }

    case "popupState": {
      void describe(message.tabId).then(respond);
      return true;
    }

    case "orphan": {
      // The page has a player and nothing on the wire explained it. `probePage` applies
      // the same capture, denylist and opt-out checks as every other channel, and the
      // daemon's extractor is what actually looks at the URL.
      if (tabId === undefined || !MEDIA_CAPTURE) return false;
      void media.probePage(message.pageUrl, tabId);
      return false;
    }

    case "framePlayer": {
      // The same signal as `orphan`, from a frame the top document cannot see into
      // (03 §5). The page is named by the *tab*, never by the message: `sender.tab.url`
      // is the top-level URL the user is actually on, the browser fills it in, and a
      // subframe — routinely someone else's code — cannot influence it. `probePage`
      // then applies the identical capture, denylist and opt-out checks, and drops the
      // ask outright if a ladder is already known for the tab.
      if (tabId === undefined || !MEDIA_CAPTURE) return false;
      const pageUrl = sender.tab?.url;
      if (!pageUrl) return false;
      void media.probePage(pageUrl, tabId);
      return false;
    }

    case "mse": {
      // Metadata only, and only ever a hint: the URLs the page fetched that channel 3
      // could not attribute. They go through the same probe as anything else, so a wrong
      // guess costs one parse and produces no overlay.
      if (tabId === undefined || !MEDIA_CAPTURE) return false;
      for (const url of message.signal.urls.slice(0, 4)) {
        media.inspect({
          url,
          tabId,
          type: "media",
          observed: { mimeType: undefined },
        });
      }
      return false;
    }

    default:
      return false;
  }
}

/**
 * Hands a chosen rung to the daemon.
 *
 * `tab` is the browser's own account of the sender, which a content script has and the
 * popup does not — a popup is a document of its own, in no tab, so the page it is talking
 * about has to be looked up from the id it named.
 */
async function submit(
  message: Extract<Internal, { kind: "download" }>,
  tabId: number,
  tab: { url?: string; title?: string } | undefined,
): Promise<void> {
  const page = tab?.url
    ? { pageUrl: tab.url, pageTitle: tab.title }
    : await describeTab(tabId);
  host.send({
    cmd: "submit",
    spec: {
      envelope: {
        url: message.selection.manifestUrl,
        method: "GET",
        headers: [],
        tabId,
        pageUrl: page.pageUrl,
        pageTitle: message.pageTitle || page.pageTitle,
        capturedAt: Date.now(),
      },
      category: "Video",
      priority: "Normal",
      startPaused: false,
      media: message.selection,
    },
  });
}

/**
 * Switches capture on or off for one origin.
 *
 * Recorded in the daemon's settings rather than in extension storage, because the same
 * list governs capture in the desktop app's settings screen. One opt-out, one place,
 * visible in all three of the places a user might look for it.
 *
 * Turning it back **on** is the direction that did not exist until the popup did. The
 * overlay is the only thing that could have offered it and it is exactly what the switch
 * removed from the page, so an origin on this list used to be a one-way door with no sign
 * on it — every capture path returning silently, for good, with nothing anywhere saying
 * why (03 §The overlay).
 */
async function setCapture(origin: string, on: boolean): Promise<void> {
  const config = await settings.full();
  // No settings means no daemon, and the daemon owns this list.
  if (!config) return;
  const off = config.siteOptouts.includes(origin);
  if (on !== off) return;
  const updated = {
    ...config,
    siteOptouts: on
      ? config.siteOptouts.filter((site) => site !== origin)
      : [...config.siteOptouts, origin],
  };
  settings.adopt(updated);
  host.send({ cmd: "setSettings", settings: updated });
}

/**
 * Everything the popup draws, in one answer.
 *
 * One round trip rather than four. The popup is opened, read and dismissed in a couple of
 * seconds, and a panel that fills itself in piece by piece in that window reads as broken
 * rather than as fast.
 */
async function describe(tabId: number): Promise<PopupState> {
  const [config, tab, daemon, candidates] = await Promise.all([
    settings.current(),
    describeTab(tabId),
    host.status(),
    MEDIA_CAPTURE ? media.known(tabId) : Promise.resolve([]),
  ]);
  return {
    origin: originOf(tab.pageUrl),
    pageTitle: tab.pageTitle ?? "",
    optedOut: settings.optedOut(config, tab.pageUrl),
    captureOff: !config.enableCapture,
    daemon,
    candidates,
  };
}

/** The origin the switch acts on, or `null` where there is nothing to switch. */
function originOf(url: string | undefined): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    // `chrome://`, `about:` and `file:` have an origin in name only, and offering to
    // change a setting for one would be offering a control that does nothing.
    return /^https?:$/.test(parsed.protocol) ? parsed.origin : null;
  } catch {
    return null;
  }
}
