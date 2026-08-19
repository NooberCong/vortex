/**
 * Channel 2 — download takeover (03 §2).
 *
 * The browser decides something is a file; Vortex asks to move it instead. The order of
 * the checks below is the whole design, and it is defensive on purpose:
 *
 * ```text
 * downloads.onCreated(item)
 *    ├── DRM denylist?              → do nothing
 *    ├── not http(s), or too small? → do nothing
 *    ├── capture off for this site? → do nothing
 *    ├── daemon reachable RIGHT NOW?→ if not, DO NOTHING
 *    ├── still running?             → it may have finished already; if so, do nothing
 *    ├── does the daemon get bytes? → probe first; if not, do nothing
 *    ├── downloads.cancel
 *    ├── did the cancel take?       → if it finished under us, do nothing
 *    ├── downloads.erase
 *    └── Submit
 * ```
 *
 * The first three checks are `handoff.wanted`, shared with `intercept.ts` — this same
 * channel one step earlier on Firefox, where the response can be taken before a download
 * exists. Everything it declines arrives here.
 *
 * **The non-negotiable rule is the third one from the bottom.** A download manager that
 * eats the user's download because its own service crashed is worse than no download
 * manager. Every branch above that ends in "do nothing", and "do nothing" means the user
 * sees an ordinary browser download and never learns Vortex considered it.
 *
 * The probe comes *before* the cancel for the same reason. Re-issuing a browser request
 * from another process fails on single-use, session-bound and fingerprint-gated URLs, and
 * the only honest way to find out is to try. A `Range: bytes=0-0` costs one round trip; a
 * failed takeover after `erase` costs the user their download.
 *
 * The `running` checks are the same rule applied to time. `DownloadItem` is a snapshot from
 * `onCreated`, and a small file can be on disk before the probe comes back — at which point
 * `cancel` succeeds without cancelling anything, because cancelling a finished download is
 * a no-op rather than an error. Erasing then would hide a file the browser had already
 * written, and submitting would fetch it a second time.
 */

import { browser } from "wxt/browser";

import type { JobSpec, RequestEnvelope } from "@vortex/proto";
import { describeTab, synthesise } from "./envelope";
import * as handoff from "./handoff";
import * as host from "./host";
import * as ledger from "./ledger";

/**
 * How long to wait for `onDeterminingFilename` after `onCreated`.
 *
 * That event yields the post-redirect URL and the browser's own resolved filename, which
 * is strictly better than anything derivable from the URL. It fires within a tick in
 * Chrome and never in Firefox, so the wait is short and its absence costs only the better
 * name.
 */
const FILENAME_GRACE = 400;

/** Resolved filenames from `onDeterminingFilename`, keyed by download id. */
const named = new Map<number, { filename: string; finalUrl?: string }>();
const waiting = new Map<number, (value: void) => void>();

export function watch(): void {
  browser.downloads.onCreated.addListener((item) => {
    void consider(item);
  });

  // Chrome only. It must call `suggest()` or the download hangs forever — including the
  // ones we are about to leave alone, which is most of them.
  const determining = browser.downloads.onDeterminingFilename;
  determining?.addListener((item, suggest) => {
    named.set(item.id, { filename: item.filename, finalUrl: item.finalUrl });
    waiting.get(item.id)?.();
    waiting.delete(item.id);
    suggest();
  });
}

async function consider(item: {
  id: number;
  url: string;
  finalUrl?: string;
  referrer?: string;
  filename?: string;
  mime?: string;
  totalBytes?: number;
  fileSize?: number;
  state?: string;
  incognito?: boolean;
}): Promise<void> {
  if (!(await eligible(item))) return;

  const filename = await resolvedName(item.id);
  const url = filename?.finalUrl || item.finalUrl || item.url;
  const tabId = await activeTab();

  const envelope = await compose(item, url, tabId);
  const spec: JobSpec = {
    envelope,
    filename: leafOf(filename?.filename) ?? undefined,
    priority: "Normal",
    startPaused: false,
  };

  // The filename grace alone is longer than a fast small transfer, so the item may already
  // be finished. Asking now costs one lookup and saves a pointless probe.
  if ((await stateOf(item.id)) !== "in_progress") return;

  // Probe before erasing. A non-2xx here means the browser keeps the download and the
  // user sees nothing unusual.
  if (!(await handoff.reachesBytes(envelope))) return;

  try {
    await browser.downloads.cancel(item.id);
  } catch {
    // The user cancelled it themselves, or it finished while we probed. Either way there
    // is nothing left to take over.
    return;
  }
  // `cancel` resolving is not evidence that anything was cancelled — a download that
  // finished during the probe stays `complete`, with its bytes on disk. A download this
  // call actually stopped is `interrupted`, and only that one is ours to erase and refetch.
  if ((await stateOf(item.id)) !== "interrupted") return;

  await browser.downloads.erase({ id: item.id });
  host.send({ cmd: "submit", spec });
}

async function eligible(item: {
  url: string;
  finalUrl?: string;
  referrer?: string;
  totalBytes?: number;
  fileSize?: number;
  state?: string;
  incognito?: boolean;
}): Promise<boolean> {
  // A download that is already finished has nothing left to move.
  if (item.state === "complete" || item.state === "interrupted") return false;

  const allowed = await handoff.wanted({
    url: item.finalUrl || item.url,
    referrer: item.referrer,
    size: item.totalBytes || item.fileSize,
    incognito: item.incognito,
  });
  if (!allowed) return false;

  return host.reachable();
}

/** Waits briefly for Chrome's resolved filename, and shrugs where the event doesn't exist. */
function resolvedName(id: number): Promise<{ filename: string; finalUrl?: string } | undefined> {
  const already = named.get(id);
  if (already) {
    named.delete(id);
    return Promise.resolve(already);
  }
  if (!browser.downloads.onDeterminingFilename) return Promise.resolve(undefined);

  return new Promise((resolve) => {
    const done = () => {
      clearTimeout(timer);
      waiting.delete(id);
      const found = named.get(id);
      named.delete(id);
      resolve(found);
    };
    const timer = setTimeout(done, FILENAME_GRACE);
    waiting.set(id, done);
  });
}

/**
 * The envelope to replay: the browser's own, when the ledger has it.
 *
 * The ledger version carries the exact headers the browser sent, cookies included. The
 * synthesised fallback carries a `Referer` and a reconstructed cookie header, which is
 * enough often enough to be worth trying — and the probe decides.
 */
async function compose(
  item: { url: string; mime?: string; totalBytes?: number; referrer?: string },
  url: string,
  tabId: number | undefined,
): Promise<RequestEnvelope> {
  const remembered = await ledger.lookup(tabId, item.url);
  const envelope = remembered
    ? { ...remembered, headers: [...remembered.headers] }
    : await synthesise(url, tabId);

  if (url !== envelope.url) envelope.finalUrl = url;
  envelope.mimeType ??= item.mime || undefined;
  envelope.contentLength ??= item.totalBytes && item.totalBytes > 0 ? item.totalBytes : undefined;
  envelope.tabId ??= tabId;
  envelope.capturedAt = envelope.capturedAt || Date.now();

  if (item.referrer && !envelope.headers.some(([k]) => k.toLowerCase() === "referer")) {
    envelope.headers.push(["Referer", item.referrer]);
  }

  // The page title is what names a file whose URL is a hash and whose server sent no
  // `Content-Disposition` (04 §8).
  const tab = await describeTab(tabId);
  envelope.pageUrl ??= tab.pageUrl;
  envelope.pageTitle ??= tab.pageTitle;
  return envelope;
}

/**
 * What the browser says about this download *now*.
 *
 * `DownloadItem` is a snapshot taken at `onCreated`, where `state` is always
 * `in_progress`; it says nothing about the file a second later, and the two checks in
 * `consider` are asking about two different moments. `undefined` — no such download, or no
 * answer at all — is not a state to act on either.
 */
async function stateOf(id: number): Promise<string | undefined> {
  try {
    const [found] = await browser.downloads.search({ id });
    return found?.state;
  } catch {
    return undefined;
  }
}

/**
 * Which tab's ledger to search.
 *
 * `DownloadItem` carries no tab id — the platform simply does not say where a download
 * came from. The focused tab is the answer in every case a user would recognise: they
 * clicked a link, and they are looking at the page they clicked it on. A background tab
 * that starts a download unprompted gets the synthesised envelope instead, which is the
 * right outcome for a download the user did not initiate.
 */
async function activeTab(): Promise<number | undefined> {
  try {
    const [active] = await browser.tabs.query({ active: true, currentWindow: true });
    return active?.id;
  } catch {
    // No window focused, or no `tabs` permission in this build.
    return undefined;
  }
}

/** `C:\Users\x\Downloads\a.iso` or `a/b.iso` → `b.iso`. Only the leaf is ours to choose. */
function leafOf(path: string | undefined): string | null {
  if (!path) return null;
  const leaf = path.split(/[\\/]/).pop();
  return leaf && leaf.length > 0 ? leaf : null;
}

/** Test seam. */
export function __resetForTests(): void {
  named.clear();
  waiting.clear();
}
