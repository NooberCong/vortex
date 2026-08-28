/**
 * The daemon's settings, cached for the extension's decisions.
 *
 * Capture consults three of them on every download — is capture on at all, is this origin
 * opted out, is the file above the size floor — and none of those can wait on a round
 * trip. So they are fetched once per worker wake, kept in session storage, and refreshed
 * whenever the daemon announces a change.
 *
 * The defaults matter less than they look. Nothing is ever cancelled without a live
 * `Ping`, so they only decide behaviour in the window between connecting and the first
 * reply; they are chosen to match `vortexd`'s own defaults so that window is invisible.
 * `full()` is the honest form — it returns what the daemon actually said, or `null` — and
 * anything that writes settings back has to go through it.
 */

import { browser } from "wxt/browser";

import type { Settings } from "@vortex/proto";
import * as host from "./host";
import * as session from "./session";

const KEY = "settings";

/** The subset capture reads, with `vortexd`'s defaults for the pre-connection window. */
export interface Effective {
  enableCapture: boolean;
  siteOptouts: string[];
  /** Below this, taking a download over costs more than it saves. */
  minCaptureBytes: number;
  defaultMediaHeight: number;
  subtitles: boolean;
}

const DEFAULTS: Effective = {
  enableCapture: true,
  siteOptouts: [],
  minCaptureBytes: 1024 * 1024,
  defaultMediaHeight: 1080,
  subtitles: true,
};

let memo: Settings | null = null;

/** What the daemon actually said, or `null` if it has not said anything yet. */
export async function full(): Promise<Settings | null> {
  if (memo) return memo;
  const stored = (await browser.storage.session.get(KEY))[KEY] as Settings | undefined;
  if (stored) {
    memo = stored;
    return memo;
  }
  await refresh();
  return memo;
}

/** The values capture actually reads, defaulted where the daemon has not answered. */
export async function current(): Promise<Effective> {
  const settings = await full();
  if (!settings) return { ...DEFAULTS };
  return {
    enableCapture: settings.enableCapture,
    siteOptouts: settings.siteOptouts,
    minCaptureBytes: settings.minCaptureBytes,
    defaultMediaHeight: settings.defaultMediaHeight,
    subtitles: settings.subtitles,
  };
}

/** Records what the daemon just told us. Called from the event stream and from `refresh`. */
export function adopt(settings: Settings): void {
  memo = settings;
  void session.store({ [KEY]: settings });
}

/**
 * How long to wait for settings.
 *
 * Deliberately much shorter than the reply timeout used for a probe. The daemon answers
 * `GetSettings` out of memory, so a slow answer means it is not there — and this call
 * sits in front of the takeover decision, where an unanswered request would leave the
 * browser downloading for twelve seconds before anything happened either way.
 */
const SETTINGS_TIMEOUT = 2000;

/** Asks the daemon. Silent on failure — the defaults are already a working answer. */
export async function refresh(): Promise<void> {
  const event = await host.request(
    { cmd: "getSettings" },
    (e) => e.event === "settingsChanged",
    SETTINGS_TIMEOUT,
  );
  if (event?.event === "settingsChanged") adopt(event.settings);
}

/**
 * Has the user switched Vortex off for this origin?
 *
 * Stored as origins, compared as origins. A user who opts out of `https://example.com`
 * has not opted out of `https://cdn.example.com`, and guessing otherwise would silently
 * disable capture on sites they never named.
 */
export function optedOut(settings: Effective, url: string | undefined): boolean {
  if (!url) return false;
  try {
    return settings.siteOptouts.includes(new URL(url).origin);
  } catch {
    return false;
  }
}

/** Test seam. */
export function __resetForTests(): void {
  memo = null;
}
