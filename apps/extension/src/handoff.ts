/**
 * The policy both takeover channels have to agree on, and the act they both end in.
 *
 * There are two ways a transfer can be moved to `vortexd`: the browser announces a
 * download and Vortex cancels it (`takeover.ts`), or — on Firefox, where blocking
 * `webRequest` still exists — the response is intercepted before a download is ever
 * created (`intercept.ts`). They differ in *when* they run and in what the platform tells
 * them. They must not differ in *what they are willing to take*.
 *
 * Hence this module. A denylisted origin that one channel honours and the other does not
 * is not a small inconsistency: it is the DRM policy failing on Firefox only, which is
 * exactly the kind of divergence that survives review because both halves look correct in
 * isolation.
 *
 * [`hand`] is here for the same reason and it is the same argument one step later. Both
 * channels take the browser's download away from the user; both therefore owe the user a
 * receipt, in the same words, in the same corner. A takeover that says so on Chrome and
 * says nothing on Firefox is that divergence again, in the half the user can see.
 */

import type { Event, JobSpec, RequestEnvelope } from "@vortex/proto";
import { isDenied } from "./denylist";
import * as host from "./host";
import { tell } from "./messages";
import * as settings from "./settings";

/** What either channel knows about a transfer before it decides anything. */
export interface Candidate {
  /** Post-redirect where it is known, since that is the URL that would be replayed. */
  url: string;
  /** The page it came from, for the denylist and the per-site opt-out. */
  referrer?: string;
  /** Bytes, when the server said so. Absent means *unknown*, which is not the same as small. */
  size?: number;
  incognito?: boolean;
}

/**
 * Is this a transfer Vortex wants at all?
 *
 * Everything here is a property of the download itself, decided without touching the
 * daemon and without touching the browser's state. Whether Vortex *can* take it — is the
 * daemon up, does the URL survive a replay, is the download still running — is the
 * caller's question, because the two channels answer it differently.
 */
export async function wanted(candidate: Candidate): Promise<boolean> {
  // `blob:` and `data:` resolve inside the page and cannot be re-issued from another
  // process at all; `filesystem:` and extension URLs are not ours to touch.
  if (!/^https?:/i.test(candidate.url)) return false;

  // A private-window download must not become a job in a persistent queue.
  if (candidate.incognito) return false;

  if (isDenied(candidate.url) || isDenied(candidate.referrer)) return false;

  const config = await settings.current();
  if (!config.enableCapture) return false;
  if (settings.optedOut(config, candidate.url) || settings.optedOut(config, candidate.referrer)) {
    return false;
  }

  // A known size below the floor is not worth the handoff. An *unknown* size is not a
  // reason to decline — chunked responses are exactly the large files worth taking.
  const size = candidate.size ?? 0;
  return !(size > 0 && size < config.minCaptureBytes);
}

/**
 * Does the daemon actually get bytes with this envelope?
 *
 * Re-issuing a browser request from another process fails on single-use, session-bound
 * and fingerprint-gated URLs, and the only honest way to find out is to try. A
 * `Range: bytes=0-0` costs one round trip; guessing wrong costs the user their download.
 *
 * `budget` exists because the two callers are stalling different things. Channel 2 is
 * waiting alongside a download the browser is already running, so it can afford the full
 * reply timeout. Channel 2's Firefox front is holding a response open, so it cannot.
 */
export async function reachesBytes(
  envelope: RequestEnvelope,
  budget?: number,
): Promise<boolean> {
  const answer = await host.request(
    { cmd: "probe", envelope },
    (event: Event) => {
      if (event.event === "probed") {
        return event.result.url === envelope.url || event.result.finalUrl === envelope.url;
      }
      return event.event === "error";
    },
    budget,
  );
  return answer?.event === "probed";
}

/**
 * Hands a job to the daemon, and tells the tab it came from that it went.
 *
 * The order is the point. `Submit` first, because that is the thing that actually has to
 * happen and it must not wait on a message to a page that may not be listening; and the
 * receipt only if the command went out at all — a card that says "downloading in Vortex"
 * above a `Submit` that never left the browser would be the one lie this whole feature
 * exists to stop telling.
 *
 * `tabId` is best-effort by nature. Channel 2 has no tab id at all — `DownloadItem` does
 * not carry one — and infers the focused tab; a download that began in a tab which has
 * since closed, or from a page with no content script in it, has nowhere to draw. That is
 * what the toolbar badge is for, and why it is the floor rather than this (`badge.ts`).
 */
export function hand(spec: JobSpec, tabId: number | undefined): void {
  if (!host.send({ cmd: "submit", spec })) return;
  if (tabId === undefined) return;
  void tell(tabId, { kind: "captured", filename: nameOf(spec) });
}

/**
 * The best name available for a file nobody has downloaded yet.
 *
 * Deliberately not the daemon's answer. The daemon derives a better one (04 §8) and
 * broadcasts it on `JobAdded` a moment later, but the receipt's whole value is being
 * immediate — it exists to be on screen before the user has decided the click failed. A
 * name that is usually identical and occasionally rougher, now, beats the right name after
 * the moment has passed.
 */
function nameOf(spec: JobSpec): string {
  // Channel 2's is the browser's own resolved filename, which is better than anything
  // derivable from a URL. Channel 2's Firefox front has no such event and falls through.
  if (spec.filename) return spec.filename;
  const envelope = spec.envelope;
  if (envelope.filenameHint) return envelope.filenameHint;

  const url = envelope.finalUrl || envelope.url;
  try {
    const parsed = new URL(url);
    const leaf = parsed.pathname.split("/").filter(Boolean).pop();
    // `decodeURIComponent` throws on a lone `%`, which a real URL path can carry.
    if (leaf) return decode(leaf);
    return parsed.host;
  } catch {
    return "Download";
  }
}

function decode(leaf: string): string {
  try {
    return decodeURIComponent(leaf);
  } catch {
    return leaf;
  }
}
