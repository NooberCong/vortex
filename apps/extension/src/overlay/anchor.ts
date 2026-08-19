/**
 * Which player each ladder belongs to (03 §The overlay).
 *
 * A badge that sits on a video is making a claim: *this* button downloads *that* video.
 * A floating pill in the corner of the viewport never has to be right about it — there
 * is only one of it, and it speaks for whatever the page's longest manifest is. Anchoring
 * buys the user a button where they are already looking, and costs us the pairing problem.
 *
 * `webRequest` cannot solve that problem: it sees a manifest URL and a tab id, and no
 * element. So the pairing is done in the page, from the two things a player and a manifest
 * can independently agree on:
 *
 * 1. **Duration.** A player and a manifest that agree to within two per cent are the same
 *    stream. Nothing else on a page comes that close by accident — an ad break, a trailer
 *    and a preview roll are all manifests on the same page as the feature, and they are
 *    all a different length.
 * 2. **Size order**, for whatever is left. The biggest player gets the longest stream.
 *    A guess, but a directed one, and it is the case that carries a plain page with one
 *    video and one manifest — where `video.duration` is still `NaN` because the metadata
 *    has not landed.
 *
 * Both rules take the page at its word about what a player is, so the filtering in front of
 * them matters as much as the rules themselves: a preloading element, a sound effect and a
 * decorative background loop are all `<video>` elements of a perfectly credible size, and
 * any of them will happily take the badge that belonged to the feature.
 *
 * Nothing here touches the DOM beyond reading it, and nothing here decides what to show;
 * it decides what belongs to what. `index.ts` draws the result, and `geometry.ts` works out
 * whether the place it drew it can be seen.
 */

import type { MediaCandidate } from "@vortex/proto";

/** A player, reduced to the things pairing has an opinion about. */
export interface Player {
  video: HTMLVideoElement;
  width: number;
  height: number;
  /** `video.duration`, or `null` while the player has no metadata. */
  durationSecs: number | null;
  /**
   * Painted, and something a person could be watching.
   *
   * Sites keep `<video>` elements around that nobody is meant to see: a preloading
   * element at `opacity: 0`, a decoder warm-up parked at `left: -9999px`, a `<video>`
   * whose source is a `data:audio/` blob because it is a sound effect. Each of them is
   * full-sized, connected and indistinguishable from a real player by rect alone.
   */
  shown: boolean;
  /**
   * Decoration rather than the page's subject.
   *
   * `object-fit: cover` is how a full-bleed background loop is made to fill a hero
   * section, and such a thing is routinely the largest `<video>` on the page — which is
   * exactly what the size rule below would otherwise reward. It is demoted, not excluded:
   * a site whose only player is styled that way still gets a badge.
   */
  decorative: boolean;
}

export interface Pairing {
  player: Player;
  candidate: MediaCandidate;
}

/**
 * Below this a `<video>` is a thumbnail, a background loop or a sound-effect element,
 * and a download badge on it would be both wrong and unclickable.
 */
export const MIN_WIDTH = 200;
export const MIN_HEIGHT = 120;

/** Two per cent, or two seconds, whichever is kinder to a short clip. */
function tolerance(seconds: number): number {
  return Math.max(2, seconds * 0.02);
}

/** Every player on the page, measured. */
export function playersOnPage(): Player[] {
  return [...document.querySelectorAll("video")].map((video) => {
    const rect = video.getBoundingClientRect();
    const style = look(video);
    return {
      video,
      width: rect.width,
      height: rect.height,
      // `NaN` before metadata, `Infinity` for a live stream. Neither is a duration.
      durationSecs: Number.isFinite(video.duration) ? video.duration : null,
      shown: painted(style) && onPage(rect) && !soundOnly(video),
      decorative: cropped(video, rect, style),
    };
  });
}

/** The players worth putting a badge on. */
export function usablePlayers(players: Player[]): Player[] {
  return players.filter(
    (player) =>
      player.video.isConnected &&
      player.shown &&
      player.width >= MIN_WIDTH &&
      player.height >= MIN_HEIGHT,
  );
}

/** `getComputedStyle` throws for a node in no document, and answers nothing in some. */
function look(element: Element): CSSStyleDeclaration | null {
  try {
    return getComputedStyle(element) ?? null;
  } catch {
    return null;
  }
}

/** Neither hidden nor transparent. An absent answer is the initial value, which is visible. */
function painted(style: CSSStyleDeclaration | null): boolean {
  if (!style) return true;
  if (style.visibility === "hidden") return false;
  const opacity = Number.parseFloat(style.opacity ?? "");
  return !Number.isFinite(opacity) || opacity > 0;
}

/**
 * Is this element anywhere on the document at all?
 *
 * Parking something at `left: -9999px` is the oldest way to hide it without hiding it, and
 * it survives every check that asks about `visibility` or `opacity`. Page coordinates, not
 * viewport ones: a player below the fold is off the screen and very much on the page.
 */
function onPage(rect: DOMRect): boolean {
  return rect.right + scrollX > 0 && rect.bottom + scrollY > 0;
}

/** A `<video>` playing an audio data URL is a sound effect with no picture to download. */
function soundOnly(video: HTMLVideoElement): boolean {
  return (video.currentSrc || video.src || "").startsWith("data:audio/");
}

/** How far a player's shape may drift from its source before it is being cropped. */
const CROP_TOLERANCE = 0.1;

/**
 * A full-bleed background loop, as opposed to a player that merely says `object-fit: cover`.
 *
 * `cover` alone is not the signal it looks like: YouTube's own watch player computes to
 * `object-fit: cover`, and demoting that would be demoting the most-watched player on the
 * web. What actually distinguishes decoration is that the property is *doing* something —
 * a 16:9 source poured into a 1920×400 hero band is cropped by more than half, while a
 * player whose box matches its source is styled `cover` to no effect at all.
 *
 * An intrinsic size of zero is metadata that has not landed yet. Demotion is the risky
 * direction, so an unanswered question is not a reason to take it.
 */
function cropped(
  video: HTMLVideoElement,
  rect: DOMRect,
  style: CSSStyleDeclaration | null,
): boolean {
  if (style?.objectFit !== "cover") return false;
  if (!video.videoWidth || !video.videoHeight || rect.width <= 0 || rect.height <= 0) return false;
  const source = video.videoWidth / video.videoHeight;
  return Math.abs(rect.width / rect.height - source) / source > CROP_TOLERANCE;
}

/**
 * The candidates worth offering: parsed, not live, with something to download — and not
 * already described by another one.
 *
 * The sniffer cannot tell a master playlist from one of its own rungs: both are `.m3u8`,
 * both go past on the wire, and both come back from the daemon as candidates. The rung
 * comes back as a stream of exactly one variant with no resolution and no bitrate, and it
 * has the same duration as the master, so duration pairing is a coin toss between them —
 * which is how a five-rung ladder ends up rendering as a single line reading `0 kbps`.
 *
 * There is no heuristic needed. In HLS a master names its rungs by URL, so a candidate
 * whose manifest *is* one of another candidate's variant ids is that variant, seen twice.
 */
export function usableCandidates(candidates: MediaCandidate[]): MediaCandidate[] {
  const usable = candidates.filter((candidate) => candidate.variants.length > 0 && !candidate.live);
  const rungs = new Set(
    usable.flatMap((candidate) =>
      candidate.variants.length > 1 ? candidate.variants.map((variant) => variant.id) : [],
    ),
  );
  return usable.filter((candidate) => !rungs.has(candidate.manifestUrl));
}

/**
 * Pairs ladders to players.
 *
 * Every player gets at most one candidate and every candidate at most one player. What is
 * left over is left over on purpose: a page with one player and three manifests has one
 * feature and two adverts, and putting the adverts somewhere would be inventing a second
 * download the user never asked about.
 */
export function pair(candidates: MediaCandidate[], players: Player[]): Pairing[] {
  const ladders = usableCandidates(candidates);
  const screens = usablePlayers(players);
  if (ladders.length === 0 || screens.length === 0) return [];

  const takenPlayer = new Set<HTMLVideoElement>();
  const takenLadder = new Set<string>();
  const pairs: Pairing[] = [];

  // 1. Duration, closest first, so the best agreement on the page claims its player
  //    before a looser one can take it.
  const agreed: Array<Pairing & { gap: number }> = [];
  for (const player of screens) {
    if (player.durationSecs === null) continue;
    for (const candidate of ladders) {
      const length = candidate.durationSecs;
      if (length === null || length === undefined || length <= 0) continue;
      const gap = Math.abs(player.durationSecs - length);
      if (gap <= tolerance(length)) agreed.push({ player, candidate, gap });
    }
  }
  agreed.sort((a, b) => a.gap - b.gap);
  for (const match of agreed) {
    if (takenPlayer.has(match.player.video) || takenLadder.has(match.candidate.id)) continue;
    takenPlayer.add(match.player.video);
    takenLadder.add(match.candidate.id);
    pairs.push({ player: match.player, candidate: match.candidate });
  }

  // 2. Biggest player, longest stream. On the ordinary page — one player, one manifest,
  //    no metadata yet — this is the rule that does all the work.
  //
  //    Decoration sorts behind everything real first, because area alone gets a page with
  //    a full-bleed background loop above its article exactly backwards: the loop is the
  //    biggest `<video>` on the page and the least likely thing anyone came to download.
  const restPlayers = screens
    .filter((player) => !takenPlayer.has(player.video))
    .sort(
      (a, b) =>
        Number(a.decorative) - Number(b.decorative) || b.width * b.height - a.width * a.height,
    );
  const restLadders = ladders
    .filter((candidate) => !takenLadder.has(candidate.id))
    .sort((a, b) => (b.durationSecs ?? 0) - (a.durationSecs ?? 0));
  restPlayers.forEach((player, index) => {
    const candidate = restLadders[index];
    if (candidate) pairs.push({ player, candidate });
  });

  return pairs;
}
