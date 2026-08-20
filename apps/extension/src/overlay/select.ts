/**
 * What the user picked, and what that turns into for the daemon.
 *
 * Split out of `index.ts` because two different surfaces now ask the same questions —
 * the page overlay, and the toolbar popup (`entrypoints/popup`). They render nothing
 * alike and they agree about everything that matters: which rung is the one to mark,
 * what an audio-only job is, which audio track comes along, and what all of that is
 * called on the wire.
 *
 * It is also a bundling boundary. The popup importing any of this from `index.ts` would
 * drag the overlay in with it — the custom element, the geometry, the placement loop —
 * into a document that has no page to sit on. Nothing here touches the DOM.
 */

import type { MediaCandidate, MediaSelection } from "@vortex/proto";

export interface State {
  candidate: MediaCandidate;
  /** Index into `candidate.variants`, or `-1` for the audio-only rung. */
  variant: number;
  subtitles: boolean;
  expanded: boolean;
}

/**
 * Which candidate the fallback pill speaks for when a page carries several.
 *
 * Longest wins. An ad break, a trailer and a preview roll are all manifests on the same
 * page as the feature, and they are all shorter than it. Where nothing states a duration,
 * the first confirmed stream is as good a guess as any.
 */
export function best(candidates: MediaCandidate[]): MediaCandidate | null {
  const usable = candidates.filter((c) => c.variants.length > 0 && !c.live);
  if (usable.length === 0) return null;
  return usable.reduce((winner, candidate) =>
    (candidate.durationSecs ?? 0) > (winner.durationSecs ?? 0) ? candidate : winner,
  );
}

/**
 * The rung to mark, and the one the badge names.
 *
 * The player's own height wins when there is a video actually playing, because that is
 * literally "the variant matching the user's current playback resolution". Otherwise the
 * daemon's `defaultVariant`, which it derived from the user's saved preference. Falling
 * back to the maximum is exactly the behaviour this is written to avoid.
 */
export function chooseVariant(candidate: MediaCandidate, height: number | null): number {
  if (height && height > 0) {
    let best = 0;
    let closest = Number.POSITIVE_INFINITY;
    candidate.variants.forEach((variant, index) => {
      const distance = Math.abs((variant.height ?? 0) - height);
      if (distance < closest) {
        closest = distance;
        best = index;
      }
    });
    return best;
  }
  const preferred = candidate.defaultVariant;
  if (preferred !== null && preferred !== undefined && candidate.variants[preferred]) {
    return preferred;
  }
  return 0;
}

/** What the chosen rung submits. */
export function selectionOf(state: State): MediaSelection {
  const { candidate, variant, subtitles } = state;
  const audioOnly = variant < 0;
  return {
    manifestUrl: candidate.manifestUrl,
    kind: candidate.kind,
    title: candidate.title,
    // An audio-only job is the same job with no video track; the daemon reads an empty
    // variant id as "the audio group only".
    variantId: audioOnly ? "" : (candidate.variants[variant]?.id ?? ""),
    audioId: pickAudio(candidate),
    subtitleIds: subtitles && !audioOnly ? candidate.subtitles.map((track) => track.id) : [],
    container: "Auto",
  };
}

/**
 * Carries what the user has already said across a re-render.
 *
 * A live ladder can gain and lose rungs between updates, so a kept selection is clamped
 * rather than trusted. A *different* manifest is a different video: nothing carries.
 */
export function carry(
  previous: State | null,
  candidate: MediaCandidate,
  height: number | null,
): State {
  const same = previous !== null && previous.candidate.id === candidate.id;
  return {
    candidate,
    variant: same
      ? Math.min(previous.variant, candidate.variants.length - 1)
      : chooseVariant(candidate, height),
    subtitles: previous?.subtitles ?? candidate.subtitles.length > 0,
    expanded: same ? previous.expanded : false,
  };
}

function pickAudio(candidate: MediaCandidate): string | null {
  if (candidate.audio.length === 0) return null;
  return (candidate.audio.find((track) => track.default) ?? candidate.audio[0]!).id;
}
