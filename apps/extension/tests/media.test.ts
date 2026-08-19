import { describe, expect, it } from "vitest";

import { classify } from "@/src/media";

const response = (mimeType?: string, contentLength?: number) => ({
  mimeType,
  contentLength,
});

/**
 * The sniffer's decision, which is almost entirely about *not* firing.
 *
 * Every false positive here becomes an overlay on a page with no video on it, and an
 * overlay that appears when it should not is how a browser extension stops being
 * something people keep installed.
 */
describe("what counts as a video", () => {
  it("recognises a manifest by path or by content type", () => {
    expect(classify("https://x/master.m3u8", "xmlhttprequest", response())).toBe("manifest");
    expect(classify("https://x/manifest.mpd?t=1", "xmlhttprequest", response())).toBe("manifest");
    // Some CDNs serve a manifest from a path with no extension at all.
    expect(
      classify("https://x/hls/playlist", "xmlhttprequest", response("application/x-mpegURL")),
    ).toBe("manifest");
    expect(classify("https://x/m.mpd", "xmlhttprequest", response("application/dash+xml"))).toBe(
      "manifest",
    );
  });

  it("offers a direct file only once it is big enough to be worth the trouble", () => {
    expect(classify("https://x/clip.mp4", "media", response("video/mp4", 40_000_000))).toBe(
      "media",
    );
    // A hero video or a UI sound effect. Nobody wants an overlay for this.
    expect(classify("https://x/loop.webm", "media", response("video/webm", 300_000))).toBeNull();
    // A media response with no declared length is not evidence of a big file.
    expect(classify("https://x/stream.mp4", "media", response("video/mp4"))).toBeNull();
  });

  it("does not mistake a TypeScript module for an MPEG-TS segment", () => {
    // Some dev servers still send `.ts` as `video/mp2t`. A whole afternoon of an overlay
    // hovering over localhost, and the fix is one field: a module import is a `script`
    // and a media segment never is.
    expect(classify("http://localhost:5173/src/main.ts", "script", response("video/mp2t"))).toBeNull();
    expect(classify("https://cdn/seg-00042.ts", "media", response("video/mp2t"))).toBe("segment");
  });

  it("counts fMP4 and CMAF segments too, whatever they are labelled", () => {
    expect(classify("https://cdn/chunk-0-00001.m4s", "media", response())).toBe("segment");
    expect(classify("https://cdn/v/1.cmfv", "xmlhttprequest", response())).toBe("segment");
  });

  it("says nothing about ordinary web traffic", () => {
    expect(classify("https://x/app.js", "script", response("text/javascript"))).toBeNull();
    expect(classify("https://x/a.png", "image", response("image/png", 90_000))).toBeNull();
    expect(classify("https://x/api/list", "xmlhttprequest", response("application/json"))).toBeNull();
    expect(classify("https://x/big.iso", "other", response("application/octet-stream", 5e9)))
      .toBeNull();
  });
});
