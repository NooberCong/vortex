import { browser } from "wxt/browser";

import { MEDIA_CAPTURE } from "@/src/build";
import * as capture from "@/src/capture";
import * as hook from "@/src/hook";
import * as host from "@/src/host";
import * as intercept from "@/src/intercept";
import * as media from "@/src/media";
import type { Internal } from "@/src/messages";
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

  browser.runtime.onStartup.addListener(() => {
    void settings.refresh();
    if (MEDIA_CAPTURE) void hook.restore();
  });

  // The first wake of a session is not `onStartup` — an install, an update, or an
  // eviction all land here instead, and each of them needs the settings just as much.
  void settings.refresh();
});

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
      host.send({
        cmd: "submit",
        spec: {
          envelope: {
            url: message.selection.manifestUrl,
            method: "GET",
            headers: [],
            tabId,
            pageUrl: sender.tab?.url,
            pageTitle: message.pageTitle || sender.tab?.title,
            capturedAt: Date.now(),
          },
          category: "Video",
          priority: "Normal",
          startPaused: false,
          media: message.selection,
        },
      });
      return false;
    }

    case "dismiss": {
      void dismiss(message.origin);
      return false;
    }

    case "orphan": {
      // The page has a player and nothing on the wire explained it. `probePage` applies
      // the same capture, denylist and opt-out checks as every other channel, and the
      // daemon's extractor is what actually looks at the URL.
      if (tabId === undefined || !MEDIA_CAPTURE) return false;
      void media.probePage(message.pageUrl, tabId);
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
 * The user switched the overlay off for an origin, from the overlay itself.
 *
 * It is recorded in the daemon's settings rather than in extension storage, because the
 * same list governs capture in the desktop app's settings screen. One opt-out, one place,
 * visible where the user would look for it.
 */
async function dismiss(origin: string): Promise<void> {
  const config = await settings.full();
  // No settings means no daemon, and no daemon means there was no overlay to dismiss.
  if (!config || config.siteOptouts.includes(origin)) return;
  const updated = { ...config, siteOptouts: [...config.siteOptouts, origin] };
  settings.adopt(updated);
  host.send({ cmd: "setSettings", settings: updated });
}
