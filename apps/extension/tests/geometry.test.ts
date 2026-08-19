import { afterEach, describe, expect, it } from "vitest";

import { clippedBox, clippersOf, covered, stickyHeader } from "@/src/overlay/geometry";

/** happy-dom measures everything as zero, so every rect in here is stated outright. */
function measuring<T extends Element>(
  element: T,
  box: { left: number; top: number; width: number; height: number },
): T {
  element.getBoundingClientRect = () =>
    ({
      x: box.left,
      y: box.top,
      left: box.left,
      top: box.top,
      width: box.width,
      height: box.height,
      right: box.left + box.width,
      bottom: box.top + box.height,
      toJSON: () => ({}),
    }) as DOMRect;
  return element;
}

function div(style: Partial<CSSStyleDeclaration>): HTMLDivElement {
  const node = document.createElement("div");
  Object.assign(node.style, style);
  return node;
}

/** Stacks the given elements as parent → child and puts the whole chain on the page. */
function nest(...chain: Element[]): void {
  chain.reduce((parent, child) => {
    parent.append(child);
    return child;
  });
  document.body.append(chain[0]!);
}

/**
 * happy-dom has no hit testing at all, so the page's stacking order is stated too.
 *
 * The elements are put on the page as well as into the answer — `getComputedStyle` has
 * nothing to say about a node that is in no document.
 */
function stacking(...elements: Element[]): void {
  for (const element of elements) if (!element.isConnected) document.body.append(element);
  (document as unknown as { elementsFromPoint: (x: number, y: number) => Element[] })
    .elementsFromPoint = () => [...elements];
}

afterEach(() => {
  document.body.replaceChildren();
  delete (document as unknown as { elementsFromPoint?: unknown }).elementsFromPoint;
});

describe("the ancestors that actually clip a player", () => {
  it("finds the box a carousel keeps its slides inside", () => {
    const rail = div({ overflow: "hidden" });
    const slide = div({});
    const video = document.createElement("video");
    nest(rail, slide, video);
    expect(clippersOf(video)).toEqual([rail]);
  });

  it("ignores an ancestor that does not clip", () => {
    const outer = div({});
    const video = document.createElement("video");
    nest(outer, video);
    expect(clippersOf(video)).toEqual([]);
  });

  it("does not invent clipping for a player that has left the flow", () => {
    // Nearly every custom player fills its container with `position: absolute`, and an
    // absolutely positioned box is clipped by its containing block — not by whatever
    // static ancestor happens to sit between the two. Treating `overflow: hidden` as
    // clipping regardless is how a badge ends up hidden for a reason that is not real.
    const stage = div({ overflow: "hidden" });
    const video = measuring(document.createElement("video"), {
      left: 0,
      top: 0,
      width: 640,
      height: 360,
    });
    video.style.position = "absolute";
    nest(stage, video);
    expect(clippersOf(video)).toEqual([]);
  });

  it("honours the containing block of a player that has left the flow", () => {
    // Same player, but now the clipping box is positioned, which makes it the containing
    // block — and a containing block clips.
    const stage = div({ overflow: "hidden", position: "relative" });
    const video = document.createElement("video");
    video.style.position = "absolute";
    nest(stage, video);
    expect(clippersOf(video)).toEqual([stage]);
  });

  it("stops at a fixed ancestor, which nothing above it can clip", () => {
    // A picture-in-picture player pinned to the corner of the window is laid out against
    // the viewport. The article it was lifted out of has no say in where it is drawn.
    const article = div({ overflow: "hidden" });
    const floater = div({ position: "fixed" });
    const video = document.createElement("video");
    nest(article, floater, video);
    expect(clippersOf(video)).toEqual([]);
  });

  it("collects every clipping box on the way up", () => {
    const outer = div({ overflow: "hidden" });
    const middle = div({});
    const inner = div({ overflow: "auto" });
    const video = document.createElement("video");
    nest(outer, middle, inner, video);
    expect(clippersOf(video)).toEqual([inner, outer]);
  });
});

describe("the part of a player its own page shows", () => {
  it("returns the player's own rect when nothing clips it", () => {
    const video = measuring(document.createElement("video"), {
      left: 50,
      top: 100,
      width: 854,
      height: 480,
    });
    expect(clippedBox(video, [])).toEqual({ left: 50, top: 100, right: 904, bottom: 580 });
  });

  it("cuts the rect down to the box that clips it", () => {
    // The player is 480 tall inside a card that shows 200 of it. The corner the badge is
    // pinned to is at the top, so this is the difference between a badge on the player
    // and a badge floating over the paragraph below the card.
    const card = measuring(div({ overflow: "hidden" }), {
      left: 50,
      top: 100,
      width: 854,
      height: 200,
    });
    const video = measuring(document.createElement("video"), {
      left: 50,
      top: 100,
      width: 854,
      height: 480,
    });
    nest(card, video);
    expect(clippedBox(video, [card])).toEqual({ left: 50, top: 100, right: 904, bottom: 300 });
  });

  it("collapses a player clipped out of existence", () => {
    const shut = measuring(div({ overflow: "hidden" }), {
      left: 50,
      top: 100,
      width: 854,
      height: 0,
    });
    const video = measuring(document.createElement("video"), {
      left: 50,
      top: 100,
      width: 854,
      height: 480,
    });
    nest(shut, video);
    const box = clippedBox(video, [shut]);
    expect(box.bottom - box.top).toBeLessThanOrEqual(0);
  });

  it("takes no notice of a clipper that has left the page", () => {
    const gone = measuring(div({ overflow: "hidden" }), {
      left: 0,
      top: 0,
      width: 0,
      height: 0,
    });
    const video = measuring(document.createElement("video"), {
      left: 50,
      top: 100,
      width: 854,
      height: 480,
    });
    document.body.append(video);
    expect(clippedBox(video, [gone])).toEqual({ left: 50, top: 100, right: 904, bottom: 580 });
  });
});

describe("the site's own bar across the top of the window", () => {
  const host = document.createElement("vortex-overlay");

  it("measures a fixed header stuck to the top", () => {
    const bar = measuring(div({ position: "fixed", backgroundColor: "rgb(20, 20, 20)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: 64,
    });
    document.body.append(bar);
    stacking(bar);
    expect(stickyHeader(host)).toBe(64);
  });

  it("measures a bar whose colour is painted by a child", () => {
    // YouTube's masthead: a `position: fixed` box with a transparent background and an
    // absolutely positioned child carrying the colour. Requiring one element to both pin
    // and paint misses the bar on some of the largest sites there are.
    const mast = measuring(div({ position: "fixed", backgroundColor: "rgba(0, 0, 0, 0)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: 56,
    });
    const paint = measuring(div({ position: "absolute", backgroundColor: "rgb(15, 15, 15)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: 56,
    });
    mast.append(paint);
    document.body.append(mast);
    stacking(paint, mast);
    expect(stickyHeader(host)).toBe(56);
  });

  it("ignores an opaque element that is not pinned to anything", () => {
    // The page's own background is opaque and reaches the top of the window. It is not a
    // bar, and the only thing separating the two is whether something holds it there.
    const page = measuring(div({ position: "absolute", backgroundColor: "rgb(15, 15, 15)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: 80,
    });
    document.body.append(page);
    stacking(page);
    expect(stickyHeader(host)).toBe(0);
  });

  it("says nothing where the page has no bar", () => {
    stacking();
    expect(stickyHeader(host)).toBe(0);
  });

  it("ignores a transparent bar, which covers nothing", () => {
    // A full-width fixed element with no background of its own is a layout wrapper or a
    // gradient scrim, and pushing every badge below one would be a bug on most of the web.
    const scrim = measuring(div({ position: "fixed", backgroundColor: "rgba(0, 0, 0, 0)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: 64,
    });
    document.body.append(scrim);
    stacking(scrim);
    expect(stickyHeader(host)).toBe(0);
  });

  it("ignores a fixed panel too tall to be a header", () => {
    // A cookie wall or a full-height drawer is not a bar, and treating it as one would
    // push the badge off the window rather than out from under something.
    const wall = measuring(div({ position: "fixed", backgroundColor: "rgb(20, 20, 20)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: innerHeight,
    });
    document.body.append(wall);
    stacking(wall);
    expect(stickyHeader(host)).toBe(0);
  });

  it("ignores a narrow floating widget", () => {
    const bubble = measuring(div({ position: "fixed", backgroundColor: "rgb(20, 20, 20)" }), {
      left: 0,
      top: 0,
      width: 120,
      height: 48,
    });
    document.body.append(bubble);
    stacking(bubble);
    expect(stickyHeader(host)).toBe(0);
  });

  it("ignores a bar that scrolls with the page", () => {
    const banner = measuring(div({ backgroundColor: "rgb(20, 20, 20)" }), {
      left: 0,
      top: 0,
      width: innerWidth,
      height: 64,
    });
    document.body.append(banner);
    stacking(banner);
    expect(stickyHeader(host)).toBe(0);
  });

  it("never measures the overlay's own host, which spans the whole window", () => {
    host.style.position = "fixed";
    host.style.backgroundColor = "rgb(20, 20, 20)";
    measuring(host, { left: 0, top: 0, width: innerWidth, height: 40 });
    stacking(host);
    expect(stickyHeader(host)).toBe(0);
  });
});

describe("whether anything is drawn over the badge", () => {
  const host = document.createElement("vortex-overlay");
  const video = document.createElement("video");

  it("says no when the player is what is on top", () => {
    stacking(video);
    expect(covered(100, 100, video, host)).toBe(false);
  });

  it("says no for the player's own controls", () => {
    // Every player on the web layers a controls bar, a gradient scrim and a click-catcher
    // over its own video. Counting those as cover would hide the badge on exactly the
    // sites it exists for, so only a real background colour counts.
    const controls = div({ backgroundColor: "rgba(0, 0, 0, 0)" });
    const scrim = div({});
    stacking(controls, scrim, video);
    expect(covered(100, 100, video, host)).toBe(false);
  });

  it("says yes for a modal drawn in front of it", () => {
    const modal = div({ backgroundColor: "rgb(255, 255, 255)" });
    stacking(modal, video);
    expect(covered(100, 100, video, host)).toBe(true);
  });

  it("does not count an invisible element as cover", () => {
    const ghost = div({ backgroundColor: "rgb(255, 255, 255)", visibility: "hidden" });
    const faded = div({ backgroundColor: "rgb(255, 255, 255)", opacity: "0" });
    stacking(ghost, faded, video);
    expect(covered(100, 100, video, host)).toBe(false);
  });

  it("never counts the overlay's own badge as cover", () => {
    // The badge is the topmost thing at its own centre by construction, and a closed
    // shadow root hands `elementsFromPoint` the host rather than what is inside it.
    stacking(host, video);
    expect(covered(100, 100, video, host)).toBe(false);
  });

  it("says no where the platform cannot be asked", () => {
    // No hit testing, no evidence — and hiding a badge takes evidence.
    expect(covered(100, 100, video, host)).toBe(false);
  });

  it("says no for a point that is nowhere", () => {
    stacking(div({ backgroundColor: "rgb(255, 255, 255)" }));
    expect(covered(Number.NaN, 100, video, host)).toBe(false);
  });

  it("reads the alpha of a colour and not the last number in it", () => {
    // `rgb(0, 0, 0)` is black. Reading the trailing component as an alpha would call it
    // transparent and let a black consent wall hide the badge underneath it.
    const black = div({ backgroundColor: "rgb(0, 0, 0)" });
    stacking(black, video);
    expect(covered(100, 100, video, host)).toBe(true);

    const clear = div({ backgroundColor: "rgb(0 0 0 / 0)" });
    stacking(clear, video);
    expect(covered(100, 100, video, host)).toBe(false);

    const half = div({ backgroundColor: "rgb(0 0 0 / 50%)" });
    stacking(half, video);
    expect(covered(100, 100, video, host)).toBe(true);
  });
});
