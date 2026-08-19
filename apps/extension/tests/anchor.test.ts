import { afterEach, describe, expect, it } from "vitest";

import type { MediaCandidate } from "@vortex/proto";
import {
  MIN_HEIGHT,
  MIN_WIDTH,
  pair,
  playersOnPage,
  usablePlayers,
  type Player,
} from "@/src/overlay/anchor";

const ladder = (over: Partial<MediaCandidate> = {}): MediaCandidate => ({
  id: "m1",
  manifestUrl: "https://example.com/master.m3u8",
  kind: "Hls",
  title: "Session 3 — Distributed Consensus",
  durationSecs: 2537,
  live: false,
  variants: [{ id: "v1080", height: 1080, bandwidth: 4_000_000 }],
  audio: [],
  subtitles: [],
  defaultVariant: 0,
  ...over,
});

/** A player, sized like a real one unless the test says otherwise. */
const player = (over: Partial<Omit<Player, "video">> = {}): Player => {
  const video = document.createElement("video");
  document.body.append(video);
  return {
    video,
    width: 854,
    height: 480,
    durationSecs: null,
    shown: true,
    decorative: false,
    ...over,
  };
};

describe("what the page offers as a player", () => {
  /** A `<video>` with a stated box and, where it has one, a stated intrinsic size. */
  const onPage = (
    box: { width: number; height: number; left?: number; top?: number },
    source?: { width: number; height: number },
  ): HTMLVideoElement => {
    const video = document.createElement("video");
    const left = box.left ?? 0;
    const top = box.top ?? 0;
    video.getBoundingClientRect = () =>
      ({
        x: left,
        y: top,
        left,
        top,
        width: box.width,
        height: box.height,
        right: left + box.width,
        bottom: top + box.height,
        toJSON: () => ({}),
      }) as DOMRect;
    if (source) {
      Object.defineProperty(video, "videoWidth", { value: source.width });
      Object.defineProperty(video, "videoHeight", { value: source.height });
    }
    document.body.append(video);
    return video;
  };

  afterEach(() => {
    document.body.replaceChildren();
  });

  it("calls a 16:9 source poured into a hero band decoration", () => {
    const hero = onPage({ width: 1920, height: 400 }, { width: 1920, height: 1080 });
    hero.style.objectFit = "cover";
    expect(playersOnPage()[0]?.decorative).toBe(true);
  });

  it("does not call a player decoration for saying `cover` to no effect", () => {
    // YouTube's own watch player computes to `object-fit: cover` and crops nothing by it.
    // Reading the property alone would demote the most-watched player on the web.
    const watch = onPage({ width: 1161, height: 653 }, { width: 1920, height: 1080 });
    watch.style.objectFit = "cover";
    expect(playersOnPage()[0]?.decorative).toBe(false);
  });

  it("waits for metadata before calling anything decoration", () => {
    // Intrinsic size is zero until the metadata lands, and demotion is the risky
    // direction — an unanswered question is not a reason to take it.
    const early = onPage({ width: 1920, height: 400 });
    early.style.objectFit = "cover";
    expect(playersOnPage()[0]?.decorative).toBe(false);
  });

  it("drops a player parked off the left of the page", () => {
    // `left: -9999px` is the oldest way to hide something without hiding it, and it
    // survives every check that asks about `visibility` or `opacity`.
    onPage({ width: 854, height: 480, left: -9999 - 854 });
    expect(usablePlayers(playersOnPage())).toEqual([]);
  });

  it("drops a player holding a sound effect", () => {
    const effect = onPage({ width: 854, height: 480 });
    effect.src = "data:audio/mpeg;base64,AAAA";
    expect(usablePlayers(playersOnPage())).toEqual([]);
  });

  it("drops a hidden and a fully transparent player", () => {
    onPage({ width: 854, height: 480 }).style.visibility = "hidden";
    onPage({ width: 854, height: 480, top: 600 }).style.opacity = "0";
    expect(usablePlayers(playersOnPage())).toEqual([]);
  });

  it("keeps an ordinary player", () => {
    onPage({ width: 854, height: 480 });
    expect(usablePlayers(playersOnPage())).toHaveLength(1);
  });
});

describe("pairing a ladder to the player it belongs to", () => {
  it("pins the badge to the one player on an ordinary page", () => {
    // The common case, and the one where nothing agrees on anything: the manifest was
    // parsed before the player had metadata, so there is no duration to match on.
    const only = player();
    const pairs = pair([ladder()], [only]);
    expect(pairs).toHaveLength(1);
    expect(pairs[0]!.player.video).toBe(only.video);
  });

  it("matches on duration when two players are on the page", () => {
    const feature = player({ durationSecs: 2537, width: 1280, height: 720 });
    const clip = player({ durationSecs: 96 });
    const pairs = pair([ladder({ id: "clip", durationSecs: 96 }), ladder()], [feature, clip]);

    const byVideo = new Map(pairs.map((p) => [p.player.video, p.candidate.id]));
    // Not by size, and not by order: the 96-second player gets the 96-second manifest
    // even though it is the smaller one and its manifest was listed first.
    expect(byVideo.get(feature.video)).toBe("m1");
    expect(byVideo.get(clip.video)).toBe("clip");
  });

  it("tolerates a manifest and a player disagreeing slightly", () => {
    // A player rounds, a manifest declares, and an HLS duration is the sum of its
    // segments. They are never bit-identical and they are always close.
    const off = player({ durationSecs: 2540.4 });
    expect(pair([ladder()], [off])).toHaveLength(1);
  });

  it("refuses a player that is nowhere near", () => {
    // 30 seconds against 42 minutes is an advert playing in front of the feature, and
    // duration must not claim it. The size rule still pairs them — there is one player
    // and one manifest, and the advert is playing *in* that player.
    const advert = player({ durationSecs: 30 });
    const pairs = pair([ladder()], [advert]);
    expect(pairs).toHaveLength(1);
    expect(pairs[0]!.candidate.id).toBe("m1");
  });

  it("gives the biggest player the longest stream when nothing else decides", () => {
    const big = player({ width: 1280, height: 720 });
    const small = player({ width: 400, height: 225 });
    const short = ladder({ id: "short", durationSecs: 120 });
    const long = ladder({ id: "long", durationSecs: 4000 });
    const byVideo = new Map(
      pair([short, long], [small, big]).map((p) => [p.player.video, p.candidate.id]),
    );
    expect(byVideo.get(big.video)).toBe("long");
    expect(byVideo.get(small.video)).toBe("short");
  });

  it("leaves the spare manifests alone", () => {
    // One player, three manifests: a feature and two adverts. Putting the adverts
    // somewhere would be inventing downloads the user never asked about.
    const one = player();
    const pairs = pair(
      [ladder(), ladder({ id: "ad1", durationSecs: 15 }), ladder({ id: "ad2", durationSecs: 30 })],
      [one],
    );
    expect(pairs).toHaveLength(1);
    expect(pairs[0]!.candidate.id).toBe("m1");
  });

  it("drops a manifest that is only a rung of another manifest", () => {
    // The sniffer sees the master and every media playlist it names, and the daemon
    // answers for all of them. A rung parses as one variant with no resolution and no
    // bitrate, and it carries the master's duration — so duration pairing is a coin toss,
    // and the loser is a five-rung ladder rendering as one line reading `0 kbps`.
    const master = ladder({
      variants: [
        { id: "https://example.com/720/index.m3u8", height: 720, bandwidth: 2_000_000 },
        { id: "https://example.com/1080/index.m3u8", height: 1080, bandwidth: 4_000_000 },
      ],
    });
    const rung = ladder({
      id: "rung",
      manifestUrl: "https://example.com/1080/index.m3u8",
      variants: [{ id: "https://example.com/1080/index.m3u8", height: null, bandwidth: 0 }],
    });

    const pairs = pair([rung, master], [player({ durationSecs: 2537 })]);
    expect(pairs).toHaveLength(1);
    expect(pairs[0]!.candidate.id).toBe("m1");
    expect(pairs[0]!.candidate.variants).toHaveLength(2);
  });

  it("keeps a lone single-variant stream, which is a progressive file", () => {
    // The rule only fires against a manifest that actually names rungs. A direct MP4 is
    // one variant and nobody else's, and dropping it would be dropping the download.
    const direct = ladder({
      kind: "Progressive",
      variants: [{ id: "https://example.com/movie.mp4", height: 1080, bandwidth: 4_000_000 }],
    });
    expect(pair([direct], [player()])).toHaveLength(1);
  });

  it("ignores a thumbnail", () => {
    const tiny = player({ width: MIN_WIDTH - 1, height: MIN_HEIGHT - 1 });
    expect(pair([ladder()], [tiny])).toEqual([]);
  });

  it("ignores a player nobody is meant to see", () => {
    // A preloading element at `opacity: 0`, a decoder warm-up parked off the page, a
    // `<video>` holding a sound effect. All full-sized, all connected, none of them a
    // place to put a button.
    expect(pair([ladder()], [player({ shown: false })])).toEqual([]);
  });

  it("puts the badge on the article's player, not the background loop behind it", () => {
    // The loop is the biggest `<video>` on the page by a distance, which is exactly what
    // the size rule rewards — and it is the one thing on the page nobody came to download.
    const loop = player({ width: 1920, height: 1080, decorative: true });
    const article = player({ width: 640, height: 360 });
    const pairs = pair([ladder()], [loop, article]);
    expect(pairs).toHaveLength(1);
    expect(pairs[0]!.player.video).toBe(article.video);
  });

  it("still uses a decorative player when it is the only one there is", () => {
    // Demoted, not excluded. A site that styles its one real player `object-fit: cover`
    // is a site where refusing to pair would mean refusing the download.
    const only = player({ decorative: true });
    expect(pair([ladder()], [only])).toHaveLength(1);
  });

  it("ignores a player that has been taken off the page", () => {
    const gone = player();
    gone.video.remove();
    expect(pair([ladder()], [gone])).toEqual([]);
  });

  it("offers nothing for a live stream or an empty ladder", () => {
    // The same rule the pill has always had, applied before anything is pinned: an
    // unbounded stream is not a file, and a manifest with no rungs has nothing to fetch.
    expect(pair([ladder({ live: true })], [player()])).toEqual([]);
    expect(pair([ladder({ variants: [] })], [player()])).toEqual([]);
    expect(pair([], [player()])).toEqual([]);
  });
});
