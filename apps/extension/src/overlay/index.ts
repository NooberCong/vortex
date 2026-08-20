/**
 * The page overlay (03 §The overlay).
 *
 * A small badge in the **top-right corner of the player itself**, which opens the ladder.
 * Four rules keep it from being the obnoxious thing every other video downloader ships,
 * and all four are enforced here rather than left to judgement:
 *
 * - **It appears only on confirmed streams.** The caller only ever passes candidates the
 *   daemon has already parsed and reached. Never on a hunch.
 * - **It sits on the video it downloads.** One badge per player, paired by `anchor.ts`,
 *   so a button on a video is a claim about *that* video. It introduces itself for a few
 *   seconds and then waits for the pointer to come back to the player.
 * - **Every rung is one click.** The ladder is the menu: a resolution is not selected and
 *   then confirmed, it is chosen, and choosing it starts the download. The rung that
 *   matches what is playing is marked, and it is never the biggest one — someone
 *   downloading a lecture does not want the 4 GB rung, and defaulting to the maximum is
 *   how a download manager teaches people to distrust its defaults.
 * - **It can be switched off for a site, from itself, permanently.**
 *
 * When nothing on the page can be paired to a player — an audio-only stream, a manifest
 * with no `<video>` to hang it on — the overlay falls back to the one thing it can always
 * do: a single pill in the bottom-right of the viewport.
 *
 * The root is a **closed** shadow root: page CSS cannot reach in, and page scripts cannot
 * read out. A page that could enumerate the overlay could tell that Vortex is installed,
 * and on a site that would rather it were not, that is a fingerprint worth denying.
 */

import type { MediaCandidate, MediaSelection, MediaVariant } from "@vortex/proto";
import css from "./style.css?inline";
import tokens from "@vortex/tokens/tokens.css?inline";
import {
  largestFrame,
  pair,
  playerFrames,
  playersOnPage,
  usablePlayers,
  type Pairing,
  type Player,
} from "./anchor";
import { best, carry, selectionOf, type State } from "./select";
import { codec, duration, estimate, quality } from "./format";
import { clippedBox, clippersOf, covered, stickyHeader, type Box } from "./geometry";

/** How long the overlay stays visible after a mouse move in fullscreen. */
const IDLE_AFTER = 2600;
/** How long a new badge shows itself before it goes quiet and waits for the pointer. */
const INTRO = 4000;
/** How long the badge says so after a job has been handed to the daemon. */
const ACKNOWLEDGED = 2400;
/** The badge's inset from the player's top-right corner. */
const INSET = 10;
/** The gap the panel keeps from the edge of the viewport. */
const MARGIN = 8;
/** Frames between re-pairings, for a player that arrives after its manifest did. */
const REPAIR_EVERY = 30;

// Re-exported so `@/src/overlay` stays the one name the rest of the extension and its
// tests know. Where they live is a bundling decision, not an API change.
export { best, carry, chooseVariant, selectionOf } from "./select";
export type { State } from "./select";

export interface Hooks {
  download(selection: MediaSelection, title: string): void;
  dismiss(origin: string): void;
}


/** One badge: a ladder, and the thing it is pinned to. */
interface Spot {
  /**
   * What the badge sits on: the `<video>` it downloads, or the `<iframe>` standing in for
   * a player this document cannot reach into (`anchor.ts`). `null` only for the corner
   * pill, where there is nothing on the page to sit on at all.
   *
   * Deliberately an `Element`. Everything placement does to it — its rect, its clippers,
   * the hit test over it — was already written against `Element`, so a frame works here
   * exactly as a player does and nothing below had to learn a second case.
   */
  anchor: Element | null;
  state: State;
  node: HTMLElement;
  /** When this badge appeared, so a new one can introduce itself. */
  born: number;
  /** Set while the badge is reporting that a job was queued. */
  queued: boolean;
  /** Last painted position, so a still page writes no styles at all. */
  x: number;
  y: number;
  /** The ancestors that clip this player, re-read on the repair tick (`geometry.ts`). */
  clippers: Element[];
  /** Set while something opaque is drawn over where the badge goes. */
  buried: boolean;
}

/**
 * Mounts, or re-renders, the overlay for the given candidates.
 *
 * Idempotent: calling it again with a fresh ladder updates in place rather than stacking
 * a second overlay, which matters on a single-page app where the player is replaced
 * without a navigation.
 */
export class Overlay {
  private readonly host: HTMLElement;
  private readonly root: ShadowRoot;
  private readonly view: HTMLElement;
  private spots: Spot[] = [];
  /** The last ladder the background sent, so a late-arriving player can be paired to it. */
  private candidates: MediaCandidate[] = [];
  /** The players paired at the last `show`, so the tick can tell when the page changed. */
  private tracked: HTMLVideoElement[] = [];
  private pointer = { x: -1, y: -1 };
  private frame: number | null = null;
  private ticks = 0;
  /** How far the site's own fixed bar reaches down the viewport. Re-read on the repair tick. */
  private header = 0;
  private idleTimer: ReturnType<typeof setTimeout> | null = null;
  /** A device with no pointer has no hover, so a badge that hides on unhover never returns. */
  private readonly alwaysOn =
    typeof matchMedia === "function" ? matchMedia("(hover: none)").matches : false;

  constructor(private readonly hooks: Hooks) {
    this.host = document.createElement("vortex-overlay");
    // A closed root is the point. `attachShadow` returns the only handle to it, and it is
    // never exposed on the element.
    this.root = this.host.attachShadow({ mode: "closed" });
    // `:host` loses to any page rule that names the element, and in fullscreen the host
    // lives inside the page's own player container where such a rule is plausible. These
    // few are not negotiable, so they are not left to the cascade.
    for (const [property, value] of [
      ["display", "block"],
      ["position", "fixed"],
      ["inset", "0"],
      ["pointer-events", "none"],
    ]) {
      this.host.style.setProperty(property!, value!, "important");
    }

    const style = document.createElement("style");
    style.textContent = `${tokens}\n${css}`;
    this.view = document.createElement("div");
    this.view.className = "root";
    this.root.append(style, this.view);

    document.addEventListener("fullscreenchange", this.onFullscreen, true);
    document.addEventListener("mousemove", this.onMouseMove, { passive: true });
    // Both on the document, both in the capture phase: an open panel has to close on the
    // two gestures everyone tries first, and neither of them is aimed at the panel. A
    // listener on the shadow root only sees Escape when focus is already inside it, which
    // is exactly when the user least needs the key.
    document.addEventListener("keydown", this.onKeyDown, true);
    document.addEventListener("pointerdown", this.onDismiss, true);
  }

  /** Shows a badge on every player it can pair, or hides the overlay when there is none. */
  show(candidates: MediaCandidate[]): void {
    this.candidates = candidates;
    const players = playersOnPage();
    this.tracked = usablePlayers(players).map((player) => player.video);

    const pairs = pair(candidates, players);
    if (pairs.length > 0) this.anchored(pairs);
    else this.floating(candidates);

    if (this.spots.length === 0) {
      this.hide();
      return;
    }
    this.onFullscreen();
    this.start();
  }

  hide(): void {
    this.stop();
    this.spots = [];
    this.view.replaceChildren();
    this.host.remove();
  }

  destroy(): void {
    document.removeEventListener("fullscreenchange", this.onFullscreen, true);
    document.removeEventListener("mousemove", this.onMouseMove);
    document.removeEventListener("keydown", this.onKeyDown, true);
    document.removeEventListener("pointerdown", this.onDismiss, true);
    if (this.idleTimer) clearTimeout(this.idleTimer);
    this.hide();
  }

  /**
   * Test seam.
   *
   * The root is closed on purpose, so there is no way in from the page — and a test *is*
   * the page. This hands back the badge nodes to the module's own suite and to nothing
   * else: the class is never exported to the content script by anything but its
   * constructor, and the page has no reference to the instance.
   */
  __nodesForTests(): HTMLElement[] {
    return this.spots.map((spot) => spot.node);
  }

  /**
   * Test seam: the animation frame, without the animation.
   *
   * Placement is where every measurement in `geometry.ts` is finally spent, and none of it
   * can be seen from the mounted DOM until a frame has run. Driving `requestAnimationFrame`
   * through fake timers instead would make the timing of the repair tick — which is a
   * frame count, not a duration — an implementation detail two files away from the test.
   */
  __advanceForTests(frames = 1): void {
    for (let index = 0; index < frames; index++) this.advance();
  }

  // ── Spots ───────────────────────────────────────────────────────────────

  /** One badge per paired player, reusing the badge that is already on it. */
  private anchored(pairs: Pairing[]): void {
    const next = pairs.map(({ player, candidate }) => {
      const existing = this.spots.find((spot) => spot.anchor === player.video);
      if (existing) {
        existing.state = carry(existing.state, candidate, playedHeight(player));
        // The page has just changed enough to be re-paired, so whatever clipped this
        // player a moment ago is no longer something to take on trust.
        existing.clippers = clippersOf(player.video);
        return existing;
      }
      return this.spawn(player.video, carry(null, candidate, playedHeight(player)));
    });
    this.settle(next);
  }

  /**
   * The fallback, when nothing could be paired: the pill, in the corner, for the longest
   * stream on the page. It is what the overlay used to be everywhere, and it is still the
   * right answer for an audio-only manifest or a player this code cannot see.
   */
  private floating(candidates: MediaCandidate[]): void {
    const candidate = best(candidates);
    if (!candidate) {
      this.settle([]);
      return;
    }
    // Nothing in this document could be paired, which on the modern web usually means the
    // player is in a frame rather than that there is no player. The frame's own box is
    // readable from here even when its contents are not, so the badge goes in the player's
    // top-right corner as it always would; the viewport pill is what is left when there is
    // not even a frame to point at — an audio-only manifest, say.
    const anchor: Element | null = largestFrame(playerFrames());
    const existing = this.spots.find((spot) => spot.anchor === anchor);
    if (existing) {
      existing.state = carry(existing.state, candidate, playbackHeight());
      this.settle([existing]);
      return;
    }
    this.settle([this.spawn(anchor, carry(null, candidate, playbackHeight()))]);
  }

  private spawn(anchor: Element | null, state: State): Spot {
    const node = el("div", anchor ? "spot" : "spot pill-spot");
    node.dataset.show = "false";
    return {
      anchor,
      state,
      node,
      born: now(),
      queued: false,
      x: NaN,
      y: NaN,
      clippers: anchor ? clippersOf(anchor) : [],
      buried: false,
    };
  }

  /** Adopts the new set of spots, dropping the nodes of any that are gone. */
  private settle(next: Spot[]): void {
    for (const spot of this.spots) if (!next.includes(spot)) spot.node.remove();
    this.spots = next;
    if (next.length === 0) return;
    if (!this.host.isConnected) document.documentElement.append(this.host);
    this.view.replaceChildren(...next.map((spot) => spot.node));
    for (const spot of next) this.render(spot);
  }

  // ── Rendering ───────────────────────────────────────────────────────────

  private render(spot: Spot): void {
    spot.node.replaceChildren(spot.state.expanded ? this.panel(spot) : this.badge(spot));
  }

  private badge(spot: Spot): HTMLElement {
    const button = el("button", spot.anchor ? "badge" : "badge pill");
    button.type = "button";
    button.setAttribute("aria-expanded", "false");
    button.setAttribute("aria-label", "Download this video");
    if (spot.queued) {
      button.classList.add("queued");
      button.append(text("span", "arrow", "✓"), text("span", "label", "Download queued"));
      return button;
    }
    button.append(
      text("span", "arrow", "↓"),
      text("span", "label", "Download"),
      text("span", "sep", "·"),
      text("span", "quality", rungLabel(spot.state)),
    );
    button.addEventListener("click", () => {
      spot.state.expanded = true;
      this.render(spot);
      spot.node.querySelector<HTMLElement>(".rung")?.focus();
    });
    return button;
  }

  private panel(spot: Spot): HTMLElement {
    const { candidate } = spot.state;
    const panel = el("div", "panel");
    panel.setAttribute("role", "dialog");
    panel.setAttribute("aria-label", "Download this video");

    const meta = [
      candidate.kind === "Hls" ? "HLS" : candidate.kind === "Dash" ? "DASH" : "Direct",
      duration(candidate.durationSecs),
    ].filter(Boolean);
    const heading = el("div", "heading");
    heading.append(text("div", "title", candidate.title || "Video"), text("div", "meta", meta.join(" · ")));

    // The visible way out. Escape and a click anywhere else also close it, but neither is
    // something a panel can be relied on to have taught anybody.
    const close = el("button", "close");
    close.type = "button";
    close.textContent = "×";
    close.title = "Close";
    close.setAttribute("aria-label", "Close");
    close.addEventListener("click", () => this.collapse());

    const head = el("div", "head");
    head.append(heading, close);
    panel.append(head);

    // Subtitles are a switch, not a rung, and they are above the ladder because a rung is
    // the last click: whatever is set when it is pressed is what gets downloaded.
    if (candidate.subtitles.length > 0) panel.append(el("hr"), this.subtitles(spot));

    panel.append(el("hr"));
    const ladder = el("div", "ladder");
    ladder.setAttribute("role", "group");
    ladder.setAttribute("aria-label", "Resolution");
    candidate.variants.forEach((variant, index) => {
      ladder.append(this.rung(spot, variant, index));
    });
    if (candidate.audio.length > 0 && candidate.variants.length > 0) {
      // Audio-only is a real thing people want from a lecture or a podcast, and it is
      // free to offer: it is the same job with the video track left out.
      ladder.append(this.audioOnly(spot));
    }
    panel.append(ladder);

    if (candidate.audio.length > 0) {
      // Muxing takes real time, and a job that sits at "combining" with no explanation
      // reads as a hang. Saying so up front costs one line.
      panel.append(text("div", "note", "Video and audio will be combined."));
    }

    const never = el("button", "quiet");
    never.type = "button";
    never.textContent = "Not on this site";
    never.title = "Stop offering downloads on this site";
    never.addEventListener("click", () => {
      this.hooks.dismiss(location.origin);
      this.hide();
    });
    const actions = el("div", "actions");
    actions.append(never);
    panel.append(actions);
    return panel;
  }

  /** A rung is the button. Choosing a resolution *is* the download. */
  private rung(spot: Spot, variant: MediaVariant, index: number): HTMLElement {
    const row = el("button", "rung");
    row.type = "button";
    row.append(
      text("span", "name", quality(variant)),
      text("span", "codec", codec(variant.codecLabel)),
      text("span", "size", estimate(variant.estimatedBytes)),
    );
    if (index === spot.state.variant) {
      row.classList.add("here");
      row.append(text("span", "mark", "playing"));
    }
    row.addEventListener("click", () => this.take(spot, index));
    return row;
  }

  private audioOnly(spot: Spot): HTMLElement {
    const row = el("button", "rung");
    row.type = "button";
    const track = spot.state.candidate.audio[0]!;
    row.append(
      text("span", "name", "Audio only"),
      text("span", "codec", codec(track.codecLabel)),
      text("span", "size", estimate(track.estimatedBytes)),
    );
    row.addEventListener("click", () => this.take(spot, -1));
    return row;
  }

  private subtitles(spot: Spot): HTMLElement {
    const row = el("label", "subs");
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = spot.state.subtitles;
    input.addEventListener("change", () => {
      spot.state.subtitles = input.checked;
    });
    const languages = spot.state.candidate.subtitles
      .map((track) => track.label || track.language || "Unknown")
      .join(", ");
    row.append(text("span", "label", "Subtitles"), text("span", "languages", languages), input);
    return row;
  }

  /** Hands the chosen rung to the daemon and says so, in place. */
  private take(spot: Spot, variant: number): void {
    spot.state.variant = variant;
    spot.state.expanded = false;
    this.hooks.download(selectionOf(spot.state), spot.state.candidate.title || document.title);
    spot.queued = true;
    this.render(spot);
    setTimeout(() => {
      if (!this.spots.includes(spot)) return;
      spot.queued = false;
      this.render(spot);
    }, ACKNOWLEDGED);
  }

  // ── Placement ───────────────────────────────────────────────────────────

  private start(): void {
    this.frame ??= requestAnimationFrame(this.tick);
  }

  private stop(): void {
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
  }

  /**
   * A frame's worth of following.
   *
   * A badge pinned to an element has to move with it, and there is no event for "the
   * player moved" — a sticky header, a lazy-loaded advert, a CSS transition and a
   * single-page route change all move it without a scroll or a resize. So this runs per
   * frame, reads a rect per player, and writes nothing unless something changed. It only
   * runs while a badge is mounted, and `requestAnimationFrame` stops on its own for a tab
   * the user is not looking at.
   */
  private tick = (): void => {
    this.frame = requestAnimationFrame(this.tick);
    this.advance();
  };

  /**
   * One frame's worth of following, with nothing scheduled.
   *
   * The order inside a deep frame is the whole reason it is written out rather than folded
   * together. Measuring comes first so this frame's placement already uses it; the hit test
   * comes last because the question it asks is about where the badge has just been put. Run
   * the other way round, a badge that has moved out from under a header stays hidden until
   * the next deep frame, which is a visible half-second of nothing.
   */
  private advance(): void {
    // Frame one, and every `REPAIR_EVERY` after it: a badge should not spend its first
    // half-second behind a header that was there before it arrived.
    const deep = ++this.ticks % REPAIR_EVERY === 1;
    if (deep) this.measure();

    for (const spot of this.spots) this.place(spot);
    if (!deep) return;
    for (const spot of this.spots) this.probe(spot);

    // A player that arrives after its manifest — an SPA route change, a lazily mounted
    // player — has no badge until something pairs it. Cheap to notice, twice a second.
    if (this.spots.some((spot) => spot.state.expanded)) return;
    const players = usablePlayers(playersOnPage()).map((player) => player.video);
    if (same(players, this.tracked)) return;
    this.show(this.candidates);
  }

  /**
   * The measurements too expensive to take every frame.
   *
   * Walking an ancestor chain through `getComputedStyle` and hit-testing the page's own
   * stacking order are both far too slow for sixty times a second, and neither answer
   * changes at anything like that rate: a site's header does not move, and a modal that
   * covers the player is going to keep covering it. Twice a second is what IDM settles on
   * for the same measurements, and half a second of a stale answer is invisible.
   */
  private measure(): void {
    this.header = stickyHeader(this.host);
    for (const spot of this.spots) {
      if (spot.anchor?.isConnected) spot.clippers = clippersOf(spot.anchor);
    }
  }

  /** Whether anything is drawn over where the badge has just been placed. */
  private probe(spot: Spot): void {
    const box = spot.node.firstElementChild as HTMLElement | null;
    // An open panel is not something to second-guess: the user opened it, it is on top by
    // construction, and it is wide enough that its centre is nowhere near where it hangs.
    if (!spot.anchor || !box || spot.state.expanded || !Number.isFinite(spot.x)) {
      spot.buried = false;
      return;
    }
    // The badge hangs down and to the left of the point it is placed at, so this is its own
    // middle rather than the player's — what matters is whether *it* can be seen.
    spot.buried = covered(
      spot.x - box.offsetWidth / 2,
      spot.y + box.offsetHeight / 2,
      spot.anchor,
      this.host,
    );
  }

  private place(spot: Spot): void {
    const box = spot.node.firstElementChild as HTMLElement | null;
    if (!box) {
      spot.node.dataset.show = "false";
      return;
    }

    /*
     * The corner pill has nothing to follow.
     *
     * The stylesheet parks it in the bottom-right of the viewport and it stays there, so
     * there is no rect to measure and no transform to write — but it still has to be told
     * it may be seen. It used to be told nothing: this function returned here, `data-show`
     * kept the `"false"` it was spawned with, and `.spot[data-show="false"]` is
     * `opacity: 0`. The fallback badge was mounted, positioned, and permanently invisible,
     * on exactly the pages that had nothing else to offer.
     *
     * It is shown unconditionally rather than on hover, because the intro-then-quiet rule
     * below is a trade a pill cannot make: a badge that hides comes back when the pointer
     * returns to its player, and a pill has no player to return to.
     */
    if (!spot.anchor) {
      spot.node.dataset.show = "true";
      return;
    }

    if (!spot.anchor.isConnected) {
      spot.node.dataset.show = "false";
      return;
    }
    // The player's rect, cut down to the part its own page actually shows: a player in a
    // carousel or a collapsing card extends well past the box that clips it, and the
    // corner a badge is pinned to is routinely the corner that is gone.
    const rect = clippedBox(spot.anchor, spot.clippers);
    // `offsetWidth`, not a rect: the panel animates in with a scale, and a measurement
    // that included the transform would feed its own easing back into the position.
    const width = box.offsetWidth;
    const height = box.offsetHeight;

    // The window, less whatever the site has stuck to the top of it. A badge drawn under
    // a sticky header is a badge nobody can click.
    const ceiling = this.header;

    // How much of the player is on the screen. A badge belongs *in* its player, so a
    // player showing only a sliver gets no badge rather than one floating over the page.
    const seen = Math.min(rect.bottom, innerHeight) - Math.max(rect.top, ceiling);
    const room = spot.state.expanded ? 1 : height + 2 * INSET;
    const onscreen =
      rect.right - rect.left > 0 &&
      seen >= room &&
      rect.right > 0 &&
      rect.left < innerWidth &&
      !spot.buried;

    // Shown when the pointer is on the player, while the panel is open, while a keyboard
    // has focus inside it, and for a few seconds after it first appears so that it is
    // possible to know it is there at all.
    const wanted =
      spot.state.expanded ||
      spot.queued ||
      this.alwaysOn ||
      now() - spot.born < INTRO ||
      spot.node.contains(this.root.activeElement) ||
      inside(this.pointer, rect);
    const show = String(onscreen && wanted);
    if (spot.node.dataset.show !== show) spot.node.dataset.show = show;
    if (!onscreen) return;

    // The anchor is the player's top-right corner, and everything hangs down and to the
    // left of it — so both clamps are about keeping that corner reachable.
    let x = Math.min(innerWidth - MARGIN, Math.max(width + MARGIN, rect.right - INSET));
    let y = Math.max(ceiling + MARGIN, Math.min(innerHeight - height - MARGIN, rect.top + INSET));
    // A player scrolled halfway off the top still has a corner on the screen. The badge
    // slides down into it rather than leaving with the corner it was pinned to.
    if (!spot.state.expanded) y = Math.min(y, rect.bottom - height - INSET);

    x = Math.round(x);
    y = Math.round(y);
    if (x === spot.x && y === spot.y) return;
    spot.x = x;
    spot.y = y;
    spot.node.style.transform = `translate(${x}px, ${y}px)`;
  }

  // ── Fullscreen and idleness ─────────────────────────────────────────────

  /**
   * Fullscreen paints the fullscreen element's subtree and nothing else, so an overlay
   * parented to `<html>` is simply not on the screen — no amount of `z-index` reaches the
   * top layer. The host moves into the player's own container instead, and moves back out
   * when it is over. A page that fullscreens the `<video>` itself has nowhere to put it:
   * a `<video>`'s children are fallback content and are never rendered.
   */
  private onFullscreen = (): void => {
    // `?? null`: not every implementation of this property agrees on what "none" is.
    const full = document.fullscreenElement ?? null;
    this.host.setAttribute("data-fullscreen", String(full !== null));
    this.host.setAttribute("data-idle", String(full !== null));
    if (full === null && this.idleTimer) {
      clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
    if (this.spots.length === 0) return;

    const home = full === null ? document.documentElement : full.tagName === "VIDEO" ? null : full;
    if (home === null) {
      this.host.remove();
      return;
    }
    if (this.host.parentElement !== home) home.append(this.host);
  };

  /** Escape closes the panel from anywhere, because nobody knows where focus is. */
  private onKeyDown = (event: KeyboardEvent): void => {
    if (event.key !== "Escape") return;
    // Only swallow the key when there was something to close — Escape belongs to the page
    // and to the browser, and a panel that eats it while shut is a bug in someone's player.
    if (this.collapse()) event.stopPropagation();
  };

  /** A click anywhere but on the overlay closes it, which is what every menu does. */
  private onDismiss = (event: Event): void => {
    if (event.composedPath().includes(this.host)) return;
    this.collapse();
  };

  /** Closes whatever is open. Answers whether there was anything. */
  private collapse(): boolean {
    const open = this.spots.filter((spot) => spot.state.expanded);
    for (const spot of open) {
      spot.state.expanded = false;
      this.render(spot);
    }
    return open.length > 0;
  }

  private onMouseMove = (event: MouseEvent): void => {
    this.pointer.x = event.clientX;
    this.pointer.y = event.clientY;
    if (!document.fullscreenElement) return;
    this.host.setAttribute("data-idle", "false");
    if (this.idleTimer) clearTimeout(this.idleTimer);
    this.idleTimer = setTimeout(() => {
      if (!this.spots.some((spot) => spot.state.expanded)) {
        this.host.setAttribute("data-idle", "true");
      }
    }, IDLE_AFTER);
  };
}

// ── Pure helpers, which is where the decisions live ─────────────────────────

/** What the collapsed badge says it will fetch. */
export function rungLabel(state: State): string {
  const variant = state.candidate.variants[state.variant];
  return variant ? quality(variant) : "Audio";
}



/** The height of the largest video actually playing on the page, if any. */
export function playbackHeight(): number | null {
  let best: number | null = null;
  for (const video of document.querySelectorAll("video")) {
    if (video.readyState === 0 || video.videoHeight === 0) continue;
    if (best === null || video.videoHeight > best) best = video.videoHeight;
  }
  return best;
}




/** What this player is actually showing, which is what the marked rung should match. */
function playedHeight(player: Player): number | null {
  const video = player.video;
  if (video.readyState === 0 || video.videoHeight === 0) return null;
  return video.videoHeight;
}

function inside(point: { x: number; y: number }, rect: Box): boolean {
  return (
    point.x >= rect.left && point.x <= rect.right && point.y >= rect.top && point.y <= rect.bottom
  );
}

function same<T>(a: readonly T[], b: readonly T[]): boolean {
  return a.length === b.length && a.every((item, index) => item === b[index]);
}

function now(): number {
  return performance.now();
}

// ── DOM sugar ───────────────────────────────────────────────────────────────

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  return node;
}

function text(tag: "span" | "div", className: string, content: string): HTMLElement {
  const node = el(tag, className);
  // `textContent`, never `innerHTML`: every string here came from a page-controlled
  // manifest, and the overlay is not a place to start trusting one.
  node.textContent = content;
  return node;
}
