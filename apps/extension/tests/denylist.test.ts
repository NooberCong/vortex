import { describe, expect, it } from "vitest";

import { excludeMatches, isDenied, isDeniedHost } from "@/src/denylist";

/**
 * The denylist has two failure modes and they cost very different amounts.
 *
 * Too narrow: an overlay appears on a protected site, the user clicks, and the engine
 * refuses. Embarrassing, and fixed by adding a line.
 *
 * Too wide: capture silently stops working on a domain nobody thought about — every PDF
 * on `apple.com`, every order invoice on `amazon.com` — and the user has no way to tell
 * that Vortex decided not to help. That is the one these tests are mostly about.
 */
describe("the DRM denylist", () => {
  it("covers the named services and their subdomains", () => {
    expect(isDeniedHost("netflix.com")).toBe(true);
    expect(isDeniedHost("www.netflix.com")).toBe(true);
    expect(isDeniedHost("occ-0-1234.1.nflxso.net")).toBe(false); // not listed, and shouldn't be
    expect(isDeniedHost("nflxvideo.net")).toBe(true);
    expect(isDeniedHost("ipv4-c001.nflxvideo.net")).toBe(true);
  });

  it("does not widen an entry to its parent domain", () => {
    // Every pair here is a domain where the video service is a tenant, not the landlord.
    expect(isDeniedHost("music.youtube.com")).toBe(true);
    expect(isDeniedHost("www.youtube.com")).toBe(false);
    expect(isDeniedHost("tv.apple.com")).toBe(true);
    expect(isDeniedHost("developer.apple.com")).toBe(false);
    expect(isDeniedHost("primevideo.com")).toBe(true);
    expect(isDeniedHost("www.amazon.com")).toBe(false);
  });

  it("is case- and trailing-dot-insensitive, because hostnames are", () => {
    expect(isDeniedHost("WWW.Netflix.COM")).toBe(true);
    expect(isDeniedHost("netflix.com.")).toBe(true);
    // A suffix is not a domain: `notnetflix.com` is somebody else entirely.
    expect(isDeniedHost("notnetflix.com")).toBe(false);
  });

  it("treats a URL it cannot parse as denied, and an absent one as not", () => {
    expect(isDenied("https://hulu.com/watch/1")).toBe(true);
    expect(isDenied("https://example.com/a.iso")).toBe(false);
    expect(isDenied("not a url")).toBe(true);
    // No referrer is the common case, and it is not evidence of anything.
    expect(isDenied(undefined)).toBe(false);
    expect(isDenied("")).toBe(false);
  });

  it("produces manifest patterns that match the same hosts the code does", () => {
    const patterns = excludeMatches();
    expect(patterns.length).toBeGreaterThan(10);
    for (const pattern of patterns) {
      // `*://*.example.com/*` — the shape Chrome and Firefox both accept, and the shape
      // that matches the bare domain as well as its subdomains.
      expect(pattern).toMatch(/^\*:\/\/\*\.[a-z0-9.-]+\/\*$/);
      const host = pattern.slice("*://*.".length, -"/*".length);
      expect(isDeniedHost(host), `${host} is in the manifest but not in the code`).toBe(true);
    }
  });
});
