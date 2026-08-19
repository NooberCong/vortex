/**
 * The extension's own internal messages.
 *
 * Everything that crosses to `vortexd` is a generated `Command`/`Event` from
 * `@vortex/proto`. These are the shapes that stay inside the browser — background to
 * overlay, page to content script — and they are deliberately few. A message here means
 * one of the halves cannot see something the other can: the overlay cannot reach the
 * daemon, and the page's `MediaSource` cannot be observed from an isolated world.
 */

import type { MediaCandidate, MediaSelection } from "@vortex/proto";

/** Background → content script. */
export type ToPage =
  | { kind: "candidates"; candidates: MediaCandidate[] }
  /**
   * Re-acquire `url` in page context (03 §Handoff 3). The content script answers with the
   * URL the browser ended up at, or `null` if it could not get one.
   */
  | { kind: "renew"; url: string };

/** Content script → background. */
export type FromPage =
  | { kind: "ready" }
  | { kind: "download"; selection: MediaSelection; pageTitle: string }
  | { kind: "dismiss"; origin: string }
  /**
   * A real player has been loaded on this page long enough that every other channel
   * would have produced a ladder by now, and none did (03 §5). The page URL is offered
   * to the daemon's extractor, which is the only thing left that can look at it.
   */
  | { kind: "orphan"; pageUrl: string };

/**
 * MAIN-world hook → isolated content script, over `window.postMessage`.
 *
 * Metadata only, never bytes (03 §4). Its real value is answering "is a video actually
 * playing right now, and which one", so the overlay can be right instead of eager.
 */
export interface MseSignal {
  /** Discriminator, because `window.postMessage` is a channel anyone can write to. */
  source: "vortex-mse";
  /** Codec strings from `addSourceBuffer` — the ladder needs them and the manifest may be hidden. */
  codecs: string[];
  /** URLs the page fetched that look like segments or manifests. */
  urls: string[];
  /** Whether any `SourceBuffer` has actually been appended to. */
  playing: boolean;
}

/** Content script → background, relaying the hook. */
export interface MseReport {
  kind: "mse";
  signal: MseSignal;
}

export type Internal = FromPage | MseReport;
