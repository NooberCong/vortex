/**
 * Channel 3 — manifest sniffing (03 §3).
 *
 * A video is not a download; it is a manifest that describes a few hundred of them. This
 * module notices that a manifest went past, hands it to `vortexd` for parsing, and gives
 * the resulting ladder to the overlay. It never fetches a manifest itself — the daemon
 * has the parsers, and asking the browser to fetch it twice would double the load on the
 * origin for no gain.
 *
 * Three signals, in descending order of confidence:
 *
 * 1. **A manifest** — `.m3u8`/`.mpd` by path or by `Content-Type`. Certain.
 * 2. **Direct media** — a `video/*` or `audio/*` body above the size floor. Certain, and
 *    the simplest case: no ladder, one file.
 * 3. **A segment burst** — `.ts`/`.m4s`/`.cmfv` arriving in numbers with no manifest in
 *    sight. Not a download in itself, but a strong signal that the manifest was fetched
 *    somewhere `webRequest` cannot see, which is what the MSE hook is for.
 *
 * Everything here is absent from the store build (`MEDIA_CAPTURE`).
 */

import { browser } from "wxt/browser";

import type { MediaCandidate, RequestEnvelope } from "@vortex/proto";
import type { Observed } from "./capture";
import { isDenied } from "./denylist";
import { describeTab, synthesise } from "./envelope";
import * as hook from "./hook";
import * as host from "./host";
import * as ledger from "./ledger";
import type { ToPage } from "./messages";
import * as settings from "./settings";

const MANIFEST_PATH = /\.(m3u8|mpd)(\?|#|$)/i;
const MANIFEST_MIME = new Set([
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
  "application/dash+xml",
  "video/vnd.mpeg.dash.mpd",
]);
const SEGMENT_PATH = /\.(ts|m4s|cmfv|cmfa|fmp4)(\?|#|$)/i;

/** How many segments with no manifest before the MSE hook is worth turning on. */
const BURST_THRESHOLD = 6;
/** Below this, a `video/*` body is a background loop or a sound effect, not a download. */
const DIRECT_MEDIA_FLOOR = 4 * 1024 * 1024;

/**
 * Manifests already sent for parsing: URL → when the probe went out, or `ANSWERED` once a
 * ladder came back for it.
 *
 * A de-duplicator, so a player that re-fetches its manifest every few seconds does not
 * re-probe — but deliberately not a *permanent* one for the failures. A probe can go
 * nowhere for reasons that have nothing to do with the URL: the daemon was restarting, the
 * port was dead until the next liveness alarm, the origin refused a request it had already
 * served the browser once. Recording the URL as tried on that basis retires it for the life
 * of the worker, and the manifest the player is still happily re-fetching is never asked
 * about again — a silent, permanent failure produced by a transient one.
 *
 * Answered manifests are pinned instead of expiring, because those genuinely never need
 * asking about twice.
 */
const probed = new Map<string, number>();

/** A manifest that produced a ladder. Never probed again. */
const ANSWERED = Number.POSITIVE_INFINITY;

/**
 * How long a probe that has not produced a ladder holds its URL before another may go out.
 *
 * Long enough that the daemon's own fetch, parse and reply have had several times over what
 * they need, so a retry means "that went nowhere" rather than "that has not finished".
 */
export const RETRY_AFTER = 30_000;
/** Segment counts per tab, reset on navigation. */
const bursts = new Map<number, number>();

/** The ladders currently known per tab, so a re-injected overlay can ask for them again. */
const candidatesKey = (tabId: number) => `media:${tabId}`;

export function listen(): void {
  host.onEvent((event) => {
    if (event.event !== "mediaFound") return;
    void deliver(event.tab, event.candidates);
  });

  browser.tabs.onRemoved.addListener((tabId) => {
    bursts.delete(tabId);
    void browser.storage.session.remove(candidatesKey(tabId));
  });

  // A navigation invalidates everything known about the tab. Without this the overlay
  // would offer the previous page's video on the next one.
  browser.tabs.onUpdated.addListener((tabId, change) => {
    if (change.status !== "loading" || !change.url) return;
    bursts.delete(tabId);
    void browser.storage.session.remove(candidatesKey(tabId));
  });
}

/** What a response looked like. `null` means "nothing to do with video". */
export type Signal = "manifest" | "media" | "segment" | null;

/**
 * Reads one response. Pure, because this is where the false positives live and a false
 * positive here becomes an overlay on a page with no video on it.
 */
export function classify(url: string, type: string, observed: Observed): Signal {
  const mime = observed.mimeType?.toLowerCase();
  if (MANIFEST_PATH.test(url) || (mime && MANIFEST_MIME.has(mime))) return "manifest";

  const isSegmentMime = mime === "video/mp2t" || mime === "video/iso.segment";
  if (mime && /^(video|audio)\//.test(mime) && !isSegmentMime) {
    // The size floor is doing real work: every site with a hero video, an autoplaying
    // background loop or a sound effect would otherwise raise an overlay.
    return (observed.contentLength ?? 0) >= DIRECT_MEDIA_FLOOR ? "media" : null;
  }

  // `.ts` is also the extension of a TypeScript module, and some dev servers still send
  // it as `video/mp2t`. A module import is resource type `script`; a media segment never
  // is, so the type is the discriminator that keeps a local dev server quiet.
  if (type !== "script" && (SEGMENT_PATH.test(url) || isSegmentMime)) return "segment";

  return null;
}

/** Has this URL been probed recently enough that another ask would be a duplicate? */
function awaitingAnswer(url: string): boolean {
  const at = probed.get(url);
  // `ANSWERED` is `Infinity`, so the subtraction is `-Infinity` and the URL is held for
  // good — which is the intent, spelled without a second branch to keep in step.
  return at !== undefined && Date.now() - at < RETRY_AFTER;
}

/** Called for every response with headers. Cheap, and it has to be. */
export function inspect(details: {
  url: string;
  tabId: number;
  type: string;
  observed: Observed;
}): void {
  const { url, tabId, type, observed } = details;
  if (tabId < 0 || isDenied(url)) return;

  switch (classify(url, type, observed)) {
    case "manifest":
    case "media":
      void probe(url, tabId);
      return;
    case "segment": {
      const seen = (bursts.get(tabId) ?? 0) + 1;
      bursts.set(tabId, seen);
      if (seen === BURST_THRESHOLD) void hook.enable(url, tabId);
      return;
    }
    default:
      return;
  }
}

/**
 * Channel 5 — offers the page itself to the daemon's extractor (03 §5).
 *
 * Everything above this watches the network and assumes a page that plays video fetches
 * something recognisable as video. YouTube does not: no `.m3u8`, no `.mpd`, the stream
 * description embedded in the document as JSON, and the media itself arriving from
 * `videoplayback?…` — no extension for `SEGMENT_PATH` to match, in ranges under the direct
 * media floor. All four channels miss, so the extractor is handed the page URL instead.
 *
 * The page asks for this, and only after a real player has sat there long enough that any
 * of the faster channels would have answered. This re-checks that nothing has: the content
 * script's view can be a few seconds old, and an extraction is a subprocess.
 */
export async function probePage(pageUrl: string, tabId: number): Promise<void> {
  if (tabId < 0) return;
  // The daemon fetches this URL. Anything it cannot fetch has no business being sent.
  if (!/^https?:\/\//i.test(pageUrl)) return;
  // `probe` checks the *tab's* URL against the denylist, which is the same thing on every
  // page but one: this is the only channel where the URL being sent is itself the page,
  // and a guarantee should not depend on a tab lookup succeeding.
  if (isDenied(pageUrl)) return;
  if ((await known(tabId)).length > 0) return;
  await probe(pageUrl, tabId);
}

/** Asks the daemon what is in a manifest. */
async function probe(url: string, tabId: number): Promise<void> {
  if (awaitingAnswer(url)) return;
  const config = await settings.current();
  if (!config.enableCapture) return;

  const tab = await describeTab(tabId);
  if (isDenied(tab.pageUrl) || settings.optedOut(config, tab.pageUrl)) return;

  probed.set(url, Date.now());
  // A hard ceiling, because a player that re-mints a manifest URL every few minutes would
  // otherwise grow the map without bound for the life of the worker. Clearing wholesale
  // loses the `ANSWERED` marks too, which costs one repeat probe each and nothing else.
  if (probed.size > 500) probed.clear();

  const envelope = await envelopeFor(url, tabId, tab);
  host.send({ cmd: "probeMedia", envelope });
}

async function envelopeFor(
  url: string,
  tabId: number,
  tab: { pageUrl?: string; pageTitle?: string },
): Promise<RequestEnvelope> {
  const remembered = await ledger.lookup(tabId, url);
  const envelope = remembered ?? (await synthesise(url, tabId));
  envelope.tabId = tabId;
  envelope.pageUrl ??= tab.pageUrl;
  envelope.pageTitle ??= tab.pageTitle;
  return envelope;
}

/**
 * Hands a parsed ladder to the tab's overlay.
 *
 * `MediaFound` only arrives for manifests the daemon could actually parse and reach, so
 * by the time anything is delivered the stream is confirmed — which is the rule that
 * keeps the overlay from appearing on a hunch (03 §The overlay).
 */
async function deliver(tabId: number, candidates: MediaCandidate[]): Promise<void> {
  if (candidates.length === 0) return;

  // This manifest is answered, and an answered manifest is never worth asking about again
  // — which is what keeps `RETRY_AFTER` from re-probing the live streams that are working.
  for (const candidate of candidates) probed.set(candidate.manifestUrl, ANSWERED);

  const key = candidatesKey(tabId);
  const stored = ((await browser.storage.session.get(key))[key] ?? []) as MediaCandidate[];
  const merged = [...stored];
  for (const candidate of candidates) {
    const at = merged.findIndex((existing) => existing.manifestUrl === candidate.manifestUrl);
    if (at >= 0) merged[at] = candidate;
    else merged.push(candidate);
  }
  await browser.storage.session.set({ [key]: merged });
  await send(tabId, { kind: "candidates", candidates: merged });
}

/** What a freshly-injected overlay asks for, after a worker restart or an SPA navigation. */
export async function known(tabId: number): Promise<MediaCandidate[]> {
  const key = candidatesKey(tabId);
  return ((await browser.storage.session.get(key))[key] ?? []) as MediaCandidate[];
}

export async function send(tabId: number, message: ToPage): Promise<void> {
  try {
    await browser.tabs.sendMessage(tabId, message);
  } catch {
    // No overlay in that tab: it is on the denylist's `exclude_matches`, or the tab is
    // showing a PDF or an internal page. Nothing to do and nothing to report.
  }
}

/** Test seam. */
export function __resetForTests(): void {
  probed.clear();
  bursts.clear();
}
