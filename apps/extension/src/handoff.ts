/**
 * The policy both takeover channels have to agree on.
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
 */

import type { Event, RequestEnvelope } from "@vortex/proto";
import { isDenied } from "./denylist";
import * as host from "./host";
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
