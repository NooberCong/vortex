import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Orphans, SETTLE } from "@/src/orphan";

/** A `<video>` with a stated box and, unless the test says otherwise, a loaded stream. */
function playerOnPage(
  over: {
    width?: number;
    height?: number;
    readyState?: number;
    duration?: number;
    objectFit?: string;
    videoSize?: [number, number];
  } = {},
): HTMLVideoElement {
  const video = document.createElement("video");
  const width = over.width ?? 854;
  const height = over.height ?? 480;
  video.getBoundingClientRect = () =>
    ({
      x: 0,
      y: 0,
      left: 0,
      top: 0,
      width,
      height,
      right: width,
      bottom: height,
      toJSON: () => ({}),
    }) as DOMRect;
  Object.defineProperty(video, "readyState", { value: over.readyState ?? 1 });
  Object.defineProperty(video, "duration", { value: over.duration ?? 634 });
  if (over.videoSize) {
    Object.defineProperty(video, "videoWidth", { value: over.videoSize[0] });
    Object.defineProperty(video, "videoHeight", { value: over.videoSize[1] });
  }
  if (over.objectFit) video.style.objectFit = over.objectFit;
  document.body.append(video);
  return video;
}

/** happy-dom's own control surface, which its ambient types do not declare. */
function at(url: string): void {
  (window as unknown as { happyDOM?: { setURL(url: string): void } }).happyDOM?.setURL(url);
}

/** Runs the watcher past its settling period without waiting for it. */
function past(watcher: Orphans): void {
  watcher.look();
  vi.advanceTimersByTime(SETTLE + 1);
  watcher.look();
}

describe("offering a page to the extractor when nothing on the wire explained it", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // happy-dom starts at `about:blank`, and a page the daemon cannot fetch never asks.
    at("https://www.youtube.com/watch?v=aqz-KE-bpKQ");
  });

  afterEach(() => {
    vi.useRealTimers();
    document.body.replaceChildren();
  });

  it("asks once, after the faster channels have had their chance", () => {
    playerOnPage();
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    watcher.look();

    // Not immediately: an extractor run in front of an ordinary HLS site's own probe is a
    // subprocess spent on a page that was about to answer for itself.
    expect(ask).not.toHaveBeenCalled();

    vi.advanceTimersByTime(SETTLE + 1);
    watcher.look();
    expect(ask).toHaveBeenCalledOnce();
    expect(ask).toHaveBeenCalledWith("https://www.youtube.com/watch?v=aqz-KE-bpKQ");

    // And not again, however long the page sits there.
    vi.advanceTimersByTime(SETTLE * 10);
    watcher.look();
    expect(ask).toHaveBeenCalledOnce();
  });

  it("says nothing when a ladder arrived from another channel", () => {
    playerOnPage();
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    watcher.attributed();

    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing when the ladder beat it to its own first look", () => {
    // The background answers `ready` asynchronously, so a stored ladder can land before
    // this has ever run. Without the page being recorded alongside the answer, the first
    // look cannot tell "already answered" from "never armed" and starts the clock anyway.
    playerOnPage();
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    watcher.attributed();

    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing on a page with no player", () => {
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing for a player with nothing in it", () => {
    // A `<video>` element waiting for a source it will never be given. Every page that
    // preloads a player would otherwise run an extractor over itself.
    playerOnPage({ readyState: 0 });
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing for a live stream", () => {
    // `duration` is `Infinity`, which is not a file and never becomes one.
    playerOnPage({ duration: Number.POSITIVE_INFINITY });
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing for a decorative background loop", () => {
    // A hero band is not what anyone came for, and it is on an enormous number of pages.
    playerOnPage({ width: 1920, height: 400, objectFit: "cover", videoSize: [1920, 1080] });
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing for a thumbnail", () => {
    playerOnPage({ width: 120, height: 68 });
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("says nothing on a page the daemon could not fetch", () => {
    at("about:blank");
    playerOnPage();
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).not.toHaveBeenCalled();
  });

  it("re-arms on a route change, because the next video is a different video", () => {
    // YouTube never reloads the document. Without this the second video the user clicks
    // on is the first page that never gets a badge.
    playerOnPage();
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    past(watcher);
    expect(ask).toHaveBeenCalledOnce();

    at("https://www.youtube.com/watch?v=different");
    past(watcher);
    expect(ask).toHaveBeenCalledTimes(2);
    expect(ask).toHaveBeenLastCalledWith("https://www.youtube.com/watch?v=different");
  });

  it("restarts the clock on a player that is swapped out mid-settle", () => {
    // An advert finishing and the feature taking its place. The settling period exists to
    // let the other channels answer, and it has to be measured against the current player.
    const advert = playerOnPage();
    const ask = vi.fn();
    const watcher = new Orphans(ask);
    watcher.look();

    vi.advanceTimersByTime(SETTLE - 500);
    advert.remove();
    watcher.look();
    playerOnPage();
    watcher.look();

    vi.advanceTimersByTime(600);
    watcher.look();
    expect(ask, "the clock started over with the new player").not.toHaveBeenCalled();

    vi.advanceTimersByTime(SETTLE);
    watcher.look();
    expect(ask).toHaveBeenCalledOnce();
  });

  it("never looks on its own", () => {
    // The schedule belongs to the content script's `ctx`, which stops calling on
    // invalidation. A timer owned in here would keep running against a dead runtime, and
    // every `browser.runtime` call from that point throws synchronously — one uncaught
    // "Extension context invalidated" per second, in every tab the user has open.
    playerOnPage();
    const ask = vi.fn();
    new Orphans(ask);
    vi.advanceTimersByTime(SETTLE * 10);
    expect(ask).not.toHaveBeenCalled();
  });
});
