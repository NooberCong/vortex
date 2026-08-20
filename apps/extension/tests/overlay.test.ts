import { afterEach, describe, expect, it, vi } from "vitest";

import type { MediaCandidate } from "@vortex/proto";
import { bytes, codec, duration, estimate, quality } from "@/src/overlay/format";
import { best, carry, chooseVariant, Overlay, selectionOf } from "@/src/overlay";

const ladder = (over: Partial<MediaCandidate> = {}): MediaCandidate => ({
  id: "m1",
  manifestUrl: "https://example.com/master.m3u8",
  kind: "Hls",
  title: "Session 3 — Distributed Consensus",
  durationSecs: 2537,
  live: false,
  variants: [
    { id: "v2160", height: 2160, bandwidth: 12_000_000, codecLabel: "hvc1.2.4.L150" },
    { id: "v1080", height: 1080, bandwidth: 4_000_000, codecLabel: "avc1.640028" },
    { id: "v720", height: 720, bandwidth: 2_000_000, codecLabel: "avc1.4d401f" },
  ],
  audio: [{ id: "a-en", label: "English", default: true }],
  subtitles: [{ id: "s-en", label: "English", language: "en", default: true }],
  defaultVariant: 1,
  ...over,
});

describe("choosing what to download", () => {
  it("preselects what is playing, not what is biggest", () => {
    // The rule that keeps someone downloading a lecture from getting a 4 GB file they
    // did not ask for. Defaulting to the maximum is how a download manager teaches people
    // to distrust its defaults.
    expect(chooseVariant(ladder(), 720)).toBe(2);
    expect(chooseVariant(ladder(), 1080)).toBe(1);
    // An odd height — a player scaled to fit a window — takes the nearest rung.
    expect(chooseVariant(ladder(), 900)).toBe(1);
  });

  it("falls back to the daemon's preference when nothing is playing", () => {
    expect(chooseVariant(ladder(), null)).toBe(1);
    // And to the top of the list when there is no preference either — which is the
    // manifest's own first entry, not its largest.
    expect(chooseVariant(ladder({ defaultVariant: null }), null)).toBe(0);
  });

  it("speaks for the longest stream on a page, not the first", () => {
    // An ad break, a trailer and a preview roll are all manifests on the same page as
    // the feature, and they are all shorter than it.
    const advert = ladder({ id: "ad", durationSecs: 30 });
    const feature = ladder({ id: "feature", durationSecs: 2537 });
    expect(best([advert, feature])?.id).toBe("feature");
    expect(best([feature, advert])?.id).toBe("feature");
  });

  it("offers nothing for a live stream or an empty ladder", () => {
    expect(best([ladder({ live: true })])).toBeNull();
    expect(best([ladder({ variants: [] })])).toBeNull();
    expect(best([])).toBeNull();
  });

  it("submits the rung, the default audio track and the subtitles", () => {
    const selection = selectionOf({
      candidate: ladder(),
      variant: 1,
      subtitles: true,
      expanded: true,
    });
    expect(selection.variantId).toBe("v1080");
    expect(selection.audioId).toBe("a-en");
    expect(selection.subtitleIds).toEqual(["s-en"]);
  });

  it("drops video and subtitles together for an audio-only job", () => {
    // Burning subtitles into an audio file is not a thing, and an empty variant id is
    // how the daemon is told "the audio group only".
    const selection = selectionOf({
      candidate: ladder(),
      variant: -1,
      subtitles: true,
      expanded: true,
    });
    expect(selection.variantId).toBe("");
    expect(selection.audioId).toBe("a-en");
    expect(selection.subtitleIds).toEqual([]);
  });
});

/** A player big enough to carry a badge. happy-dom measures everything as zero. */
function playerOnPage(width = 854, height = 480): HTMLVideoElement {
  const video = document.createElement("video");
  video.getBoundingClientRect = () =>
    ({
      width,
      height,
      top: 100,
      left: 50,
      right: 50 + width,
      bottom: 100 + height,
      x: 50,
      y: 100,
      toJSON: () => ({}),
    }) as DOMRect;
  document.body.append(video);
  return video;
}

/**
 * Frames to run for a placement to have settled.
 *
 * The first frame measures and places; the second is where a hit test taken against that
 * placement is finally acted on. Two is enough whatever the repair interval is set to.
 */
const SETTLED = 2;

/** Where the badge was last drawn, read back off the node the overlay owns. */
function drawnAt(node: HTMLElement): { x: number; y: number } | null {
  const found = /translate\((-?[\d.]+)px,\s*(-?[\d.]+)px\)/.exec(node.style.transform);
  return found ? { x: Number(found[1]), y: Number(found[2]) } : null;
}

/** Puts a box on the page at a stated rect, since happy-dom measures everything as zero. */
function boxOnPage(
  style: Partial<CSSStyleDeclaration>,
  rect: { left: number; top: number; width: number; height: number },
): HTMLDivElement {
  const node = document.createElement("div");
  Object.assign(node.style, style);
  node.getBoundingClientRect = () =>
    ({
      x: rect.left,
      y: rect.top,
      left: rect.left,
      top: rect.top,
      width: rect.width,
      height: rect.height,
      right: rect.left + rect.width,
      bottom: rect.top + rect.height,
      toJSON: () => ({}),
    }) as DOMRect;
  document.body.append(node);
  return node;
}

/**
 * happy-dom has no hit testing, so the page's stacking order is stated — topmost first.
 *
 * Answered per point, from each element's own stated rect. A stub that returned the whole
 * stack for every point would put the site's header under the badge in the corner of the
 * player, which is the one place a header provably is not.
 */
function stacking(...elements: Element[]): void {
  (
    document as unknown as { elementsFromPoint: (x: number, y: number) => Element[] }
  ).elementsFromPoint = (x, y) =>
    elements.filter((element) => {
      const rect = element.getBoundingClientRect();
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    });
}

afterEach(() => {
  document.body.replaceChildren();
  // The overlay's host hangs off `<html>`, not `<body>`, so a test that threw before it
  // could tear its overlay down would otherwise leave one for the next test to find.
  for (const stray of document.querySelectorAll("vortex-overlay")) stray.remove();
  delete (document as unknown as { elementsFromPoint?: unknown }).elementsFromPoint;
});

describe("where the badge goes", () => {
  it("pins itself to the player rather than to the corner of the window", () => {
    // The whole point of anchoring: a button on a video is a claim about *that* video,
    // and a page with two players gets two claims instead of one guess.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const [node] = overlay.__nodesForTests();
    expect(node?.className).toBe("spot");
    expect(node?.querySelector(".badge")).not.toBeNull();
    overlay.destroy();
  });

  it("falls back to the corner when there is no player to sit on", () => {
    // An audio-only manifest, or a player this code cannot see. The pill is the one thing
    // the overlay can always do, so nothing is ever unreachable.
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const [node] = overlay.__nodesForTests();
    expect(node?.className).toContain("pill-spot");
    overlay.destroy();
  });

  it("gives a player with two videos on it a badge each", () => {
    playerOnPage(1280, 720);
    playerOnPage(400, 300);
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder({ id: "a", durationSecs: 4000 }), ladder({ id: "b", durationSecs: 90 })]);
    expect(overlay.__nodesForTests()).toHaveLength(2);
    overlay.destroy();
  });

  it("starts quiet, so a page is not wearing a button it did not ask for", () => {
    // It introduces itself when it appears and then waits for the pointer to come back to
    // the player. `data-show` is what the stylesheet reads.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    expect(overlay.__nodesForTests()[0]?.dataset.show).toBe("false");
    overlay.destroy();
  });

  it("sits in the player's own top-right corner", () => {
    // The baseline the three tests below are all departures from: nothing clips it,
    // nothing is stuck to the top of the window, nothing is drawn over it.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    const [node] = overlay.__nodesForTests();
    expect(node?.dataset.show).toBe("true");
    // The player is at 50,100 and 854 wide, so its right edge is 904 — less the inset.
    expect(drawnAt(node!)).toEqual({ x: 894, y: 110 });
    overlay.destroy();
  });

  it("comes down below a header the site has stuck to the top of the window", () => {
    // A player scrolled up under a sticky nav still has its top-right corner exactly where
    // it was, and a badge pinned to it is behind the bar. The bar is measured by hit-testing
    // the top edge, because sites agree on nothing about what a header is called.
    const bar = boxOnPage(
      { position: "fixed", backgroundColor: "rgb(20, 20, 20)" },
      { left: 0, top: 0, width: innerWidth, height: 150 },
    );
    playerOnPage();
    stacking(bar);

    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    const [node] = overlay.__nodesForTests();
    expect(node?.dataset.show).toBe("true");
    // 150 of bar plus the overlay's own margin, rather than the 110 it would have taken.
    expect(drawnAt(node!)?.y).toBe(158);
    overlay.destroy();
  });

  it("goes quiet when the player is clipped down to a sliver", () => {
    // A player inside a collapsing card or a carousel has a rect that extends well past
    // the box that shows it, and the corner the badge is pinned to is the one that is gone.
    const card = boxOnPage(
      { overflow: "hidden" },
      { left: 50, top: 100, width: 854, height: 15 },
    );
    const video = playerOnPage();
    card.append(video);

    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    expect(overlay.__nodesForTests()[0]?.dataset.show).toBe("false");
    overlay.destroy();
  });

  it("goes quiet when a modal is drawn over the player", () => {
    // The player is exactly where it was and nobody can see it. A badge floating on top of
    // a consent wall is the single most obnoxious thing a downloader can do.
    const video = playerOnPage();
    const modal = boxOnPage(
      { position: "fixed", backgroundColor: "rgb(255, 255, 255)" },
      { left: 0, top: 0, width: innerWidth, height: innerHeight },
    );
    stacking(modal, video);

    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    expect(overlay.__nodesForTests()[0]?.dataset.show).toBe("false");
    overlay.destroy();
  });

  it("stays put behind the player's own controls", () => {
    // The counterpart to the modal, and the reason cover is judged on background colour
    // alone: every player on the web layers a transparent scrim over its own video, and
    // treating those as cover would hide the badge on exactly the sites it is for.
    const video = playerOnPage();
    const scrim = boxOnPage(
      { position: "absolute", backgroundColor: "rgba(0, 0, 0, 0)" },
      { left: 50, top: 100, width: 854, height: 80 },
    );
    stacking(scrim, video);

    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    expect(overlay.__nodesForTests()[0]?.dataset.show).toBe("true");
    overlay.destroy();
  });
});

describe("choosing a resolution", () => {
  const open = (overlay: Overlay): HTMLElement => {
    const [node] = overlay.__nodesForTests();
    node!.querySelector<HTMLButtonElement>(".badge")!.click();
    return node!;
  };

  it("lists every rung the manifest carries, plus the audio", () => {
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const names = [...open(overlay).querySelectorAll(".rung .name")].map((n) => n.textContent);
    expect(names).toEqual(["2160p", "1080p", "720p", "Audio only"]);
    overlay.destroy();
  });

  it("downloads the rung that was clicked, in one click", () => {
    // A resolution is not selected and then confirmed. It is chosen, and choosing it is
    // the download — which is the difference between offering a choice and burying one.
    playerOnPage();
    const download = vi.fn();
    const overlay = new Overlay({ download, dismiss: vi.fn() });
    overlay.show([ladder()]);

    const rungs = [...open(overlay).querySelectorAll<HTMLButtonElement>(".rung")];
    rungs[2]!.click();

    expect(download).toHaveBeenCalledOnce();
    expect(download.mock.calls[0]![0].variantId).toBe("v720");
    overlay.destroy();
  });

  it("takes the audio-only rung as a job with no video track", () => {
    playerOnPage();
    const download = vi.fn();
    const overlay = new Overlay({ download, dismiss: vi.fn() });
    overlay.show([ladder()]);

    [...open(overlay).querySelectorAll<HTMLButtonElement>(".rung")].at(-1)!.click();
    expect(download.mock.calls[0]![0].variantId).toBe("");
    overlay.destroy();
  });

  it("says so, in place, once the job is the daemon's problem", () => {
    // The badge is on the video, so the confirmation belongs on the video too. Sending a
    // user to another window to find out whether their click landed is not an answer.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const node = open(overlay);
    node.querySelector<HTMLButtonElement>(".rung")!.click();
    expect(node.querySelector(".badge")?.textContent).toContain("Download queued");
    overlay.destroy();
  });

  it("closes on the close button, on Escape and on a click anywhere else", () => {
    // Three ways out, because the panel opens over the thing the user was watching and
    // "click the button again" is not one of them — the badge is gone while it is open.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    const node = overlay.__nodesForTests()[0]!;

    open(overlay);
    expect(node.querySelector(".panel")).not.toBeNull();
    node.querySelector<HTMLButtonElement>(".close")!.click();
    expect(node.querySelector(".panel"), "the close button did nothing").toBeNull();

    open(overlay);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(node.querySelector(".panel"), "Escape did nothing").toBeNull();

    open(overlay);
    document.body.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, composed: true }));
    expect(node.querySelector(".panel"), "a click outside did nothing").toBeNull();

    overlay.destroy();
  });

  it("leaves Escape alone when there is nothing of its own to close", () => {
    // Escape belongs to the page and to the browser. A page overlay that swallows it
    // while shut is a bug in somebody else's player.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    const stop = vi.spyOn(escape, "stopPropagation");
    document.dispatchEvent(escape);
    expect(stop).not.toHaveBeenCalled();
    overlay.destroy();
  });

  it("keeps what the user said across a re-render, and only while it exists", () => {
    // A live ladder gains and loses rungs between updates. A kept selection is clamped
    // rather than trusted, and a different manifest is a different video: nothing carries.
    const first = carry(null, ladder(), 720);
    expect(first.variant).toBe(2);

    const shorter = ladder({ variants: ladder().variants.slice(0, 2) });
    expect(carry(first, shorter, null).variant).toBe(1);

    const other = ladder({ id: "elsewhere", defaultVariant: 0 });
    expect(carry(first, other, null).variant).toBe(0);
  });
});

describe("clearing a badge without switching the site off", () => {
  it("takes the badge off this video and leaves it off", () => {
    playerOnPage();
    const dismiss = vi.fn();
    const overlay = new Overlay({ download: vi.fn(), dismiss });
    overlay.show([ladder()]);

    const [node] = overlay.__nodesForTests();
    node!.querySelector<HTMLButtonElement>(".clear")!.click();
    expect(overlay.__nodesForTests()).toHaveLength(0);

    // The daemon is never told. This is the temporary one; `dismiss` is the other button.
    expect(dismiss).not.toHaveBeenCalled();

    // And it stays cleared through the next ladder the daemon sends, which on a live page
    // is a second or two away. A badge that came straight back would be no button at all.
    overlay.show([ladder()]);
    expect(overlay.__nodesForTests()).toHaveLength(0);
    overlay.destroy();
  });

  it("clears one badge on a page wearing two", () => {
    playerOnPage(1280, 720);
    playerOnPage(400, 300);
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    const long = ladder({ id: "a", durationSecs: 4000 });
    const short = ladder({ id: "b", durationSecs: 90 });
    overlay.show([long, short]);
    expect(overlay.__nodesForTests()).toHaveLength(2);

    overlay.__nodesForTests()[0]!.querySelector<HTMLButtonElement>(".clear")!.click();
    expect(overlay.__nodesForTests()).toHaveLength(1);
    overlay.destroy();
  });

  it("comes back for the next video, because the player is not the video", () => {
    // The reason this is keyed by the candidate. On a single-page app the `<video>`
    // outlives what is playing in it, so clearing one badge must not silence the player
    // for everything it goes on to show.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder({ id: "watching-this" })]);
    overlay.__nodesForTests()[0]!.querySelector<HTMLButtonElement>(".clear")!.click();
    expect(overlay.__nodesForTests()).toHaveLength(0);

    overlay.show([ladder({ id: "watching-that" })]);
    expect(overlay.__nodesForTests()).toHaveLength(1);
    overlay.destroy();
  });

  it("puts the way out beside the offer, not inside it", () => {
    // A button nested in a button is not something a browser will render or a screen
    // reader will read, so the two are siblings in a row that is anchored as one.
    playerOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const [node] = overlay.__nodesForTests();
    const cluster = node!.querySelector(".cluster")!;
    expect(node!.firstElementChild, "placement measures the whole row").toBe(cluster);
    expect(cluster.querySelector(".badge .clear")).toBeNull();
    expect([...cluster.children].map((child) => child.className)).toEqual(["badge", "clear"]);
    overlay.destroy();
  });
});

describe("the overlay in a page", () => {
  it("cannot be read by the page it is drawn on", () => {
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);

    const host = document.querySelector("vortex-overlay");
    expect(host, "the overlay never mounted").not.toBeNull();
    // A page that could enumerate the overlay could tell Vortex is installed. `closed`
    // means `element.shadowRoot` is null for everyone but the code that attached it.
    expect(host!.shadowRoot).toBeNull();
    overlay.destroy();
  });

  it("puts a manifest title in the document as text, never as markup", () => {
    const download = vi.fn();
    const overlay = new Overlay({ download, dismiss: vi.fn() });
    // Titles come from a manifest, which comes from the page's own server. Rendering one
    // as HTML would hand a hostile origin script execution inside the extension's root.
    overlay.show([ladder({ title: "<img src=x onerror=alert(1)>" })]);

    expect(document.querySelector("img")).toBeNull();
    expect(document.body.innerHTML).not.toContain("onerror");
    overlay.destroy();
  });

  it("leaves nothing behind when it is dismissed", () => {
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.hide();
    expect(document.querySelector("vortex-overlay")).toBeNull();
    overlay.destroy();
  });
});

describe("the overlay's numbers", () => {
  it("renders a size exactly as the daemon does", () => {
    // These come from `@vortex/proto`, whose own suite runs the case table from
    // `fmt.rs`. What is asserted here is that the overlay did not quietly grow a second
    // implementation: the row and the pill describe the same file.
    expect(bytes(0)).toBe("0 B");
    expect(bytes(999)).toBe("999 B");
    expect(bytes(1200)).toBe("1.2 kB");
    expect(bytes(1_290_000_000)).toBe("1.29 GB");
    expect(bytes(680_000_000)).toBe("680 MB");
  });

  it("marks a declared size as an estimate", () => {
    // `bandwidth × duration / 8` is a peak declaration and routinely lands well high.
    // The tilde is the difference between an estimate and a promise.
    expect(estimate(1_200_000)).toBe("~1.2 MB");
    expect(estimate(null)).toBe("");
  });

  it("writes durations the way a player does", () => {
    expect(duration(2537)).toBe("42:17");
    expect(duration(3661)).toBe("1:01:01");
    expect(duration(9)).toBe("0:09");
    expect(duration(null)).toBe("");
  });

  it("names a codec the way people do, not the way RFC 6381 does", () => {
    expect(codec("avc1.640028,mp4a.40.2")).toBe("H.264");
    expect(codec("hvc1.2.4.L150.B0")).toBe("HEVC");
    expect(codec("av01.0.08M.08")).toBe("AV1");
    expect(codec("opus")).toBe("Opus");
    expect(codec(null)).toBe("");
  });

  it("labels a rung by height, and by bitrate only when there is no height", () => {
    expect(quality({ height: 1080, bandwidth: 4_000_000 })).toBe("1080p");
    expect(quality({ height: null, bandwidth: 128_000 })).toBe("128 kbps");
  });
});

/** An `<iframe>` at a stated rect, since happy-dom measures everything as zero. */
function frameOnPage(width = 960, height = 540): HTMLIFrameElement {
  const frame = document.createElement("iframe");
  // No `src`. The frame is found by its box, never by where it points, and happy-dom
  // would go and fetch a real one.
  frame.getBoundingClientRect = () =>
    ({
      x: 40,
      y: 80,
      left: 40,
      top: 80,
      width,
      height,
      right: 40 + width,
      bottom: 80 + height,
      toJSON: () => ({}),
    }) as DOMRect;
  document.body.append(frame);
  return frame;
}

/**
 * The fallback, when nothing in this document can be paired.
 *
 * Both of these were silent: the badge was constructed, mounted and positioned, and could
 * not be seen. Nothing threw, nothing logged, and the only symptom was a page that had
 * obviously captured a stream and obviously had no button on it.
 */
describe("a stream with no player of its own in the document", () => {
  it("pins the badge to the frame the player is in", () => {
    // A cross-origin embed. `playersOnPage` finds nothing — `contentDocument` is null
    // across origins — but the frame's own box is readable, and it is exactly where the
    // player is. The badge belongs in its corner, not in the corner of the window.
    const frame = frameOnPage();
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    const [node] = overlay.__nodesForTests();
    expect(node?.className, "anchored, not the corner pill").toBe("spot");
    expect(node?.dataset.show).toBe("true");

    // Placed against the frame's rect, at its top-right corner.
    const rect = frame.getBoundingClientRect();
    const at = drawnAt(node!);
    expect(at).not.toBeNull();
    expect(at!.x).toBeLessThanOrEqual(rect.right);
    expect(at!.y).toBeGreaterThanOrEqual(rect.top);
  });

  it("shows the corner pill when there is not even a frame to point at", () => {
    // An audio-only manifest, or a player this code cannot see by any route. The pill is
    // the last thing on offer, and it used to be mounted permanently invisible: `place`
    // returned before it could set `data-show`, so the node kept the `"false"` it was
    // spawned with and `.spot[data-show="false"]` is `opacity: 0`.
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);

    const [node] = overlay.__nodesForTests();
    expect(node?.className).toBe("spot pill-spot");
    expect(node?.dataset.show, "a pill that hides can never be brought back").toBe("true");
  });

  it("keeps the pill up rather than hiding it after the intro", () => {
    // A badge hides on unhover because the pointer returning to its player brings it back.
    // A pill has no player to return to, so the same rule would simply delete it.
    const overlay = new Overlay({ download: vi.fn(), dismiss: vi.fn() });
    overlay.show([ladder()]);
    overlay.__advanceForTests(SETTLED);
    expect(overlay.__nodesForTests()[0]?.dataset.show).toBe("true");

    vi.spyOn(performance, "now").mockReturnValue(performance.now() + 60_000);
    overlay.__advanceForTests(SETTLED);
    expect(overlay.__nodesForTests()[0]?.dataset.show).toBe("true");
  });
});
