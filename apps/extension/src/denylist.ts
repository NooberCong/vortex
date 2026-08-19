/**
 * The DRM denylist (03 §The DRM denylist).
 *
 * This is the first of two independent enforcement points. Here it is a *policy*: on these
 * origins the sniffer never reports and the overlay never renders, so Vortex is invisible
 * and the browser's own player is undisturbed. The second point is in the engine, which
 * refuses a protected manifest whatever origin it came from — that one is a *guarantee*.
 *
 * Two places rather than one because they fail differently. A missing entry here is a
 * cosmetic bug: an overlay appears, the user clicks, and the engine says no. A missing
 * check in the engine would be a product-ending bug. Neither layer is the other's backup.
 */

/**
 * Registrable domains. A host matches if it is one of these or a subdomain of one.
 *
 * Kept deliberately narrow. `music.youtube.com` is here and `youtube.com` is not;
 * `tv.apple.com` is here and `apple.com` is not; `primevideo.com` is here and
 * `amazon.com` is not. Widening an entry to its parent domain would switch capture off
 * across an entire shop or developer site to protect one video catalogue, and a download
 * manager that silently stops working on Amazon is a worse bug than an overlay that
 * appears once and is refused.
 */
const DENIED = [
  "netflix.com",
  "disneyplus.com",
  "disney-plus.net",
  "primevideo.com",
  "max.com",
  "hbomax.com",
  "hulu.com",
  "tv.apple.com",
  "peacocktv.com",
  "paramountplus.com",
  "crunchyroll.com",
  "spotify.com",
  "music.youtube.com",
  "tidal.com",
  "deezer.com",
  "britbox.com",
  "iplayer.co.uk",
  "channel4.com",
  "itv.com",
  "sky.com",
  "nowtv.com",
  "canalplus.com",
  "wakanim.tv",
  "funimation.com",
  "starz.com",
  "showtime.com",
  "mubi.com",
  "curiositystream.com",
  "discoveryplus.com",
  "viaplay.com",
  "rakuten.tv",
  "videoland.com",
] as const;

/**
 * CDN and licence hosts that serve the same catalogues from a different name. A page can
 * sit on a permitted origin and still pull protected segments from one of these.
 */
const DENIED_DELIVERY = [
  "nflxvideo.net",
  "nflximg.net",
  "aiv-cdn.net",
  "aiv-delivery.net",
  "media-amazon.com",
  "dssott.com",
  "bamgrid.com",
  "hulustream.com",
  "hbomaxcdn.com",
  "cbsivideo.com",
  "scdn.co",
  "audio-fa.scdn.co",
  "vod-akc-eu.crunchyroll.com",
  "widevine.com",
  "license.vudu.com",
] as const;

const ALL = [...DENIED, ...DENIED_DELIVERY];

/** `true` when `host` is one of the denied domains or a subdomain of one. */
export function isDeniedHost(host: string): boolean {
  const lower = host.toLowerCase().replace(/\.$/, "");
  return ALL.some((domain) => lower === domain || lower.endsWith(`.${domain}`));
}

/** `true` when the URL's host is denied. An unparseable URL is treated as denied. */
export function isDenied(url: string | undefined | null): boolean {
  if (!url) return false;
  try {
    return isDeniedHost(new URL(url).hostname);
  } catch {
    // A URL the platform handed us that we cannot parse is not something to guess about.
    return true;
  }
}

/**
 * The same list as content-script `exclude_matches` patterns.
 *
 * Putting it in the manifest as well as in code is the difference between "we decided not
 * to run" and "we were never loaded". On a Netflix tab the overlay script does not exist
 * in the page at all, which is a claim that survives a code review of the built artifact.
 */
export function excludeMatches(): string[] {
  return DENIED.map((domain) => `*://*.${domain}/*`);
}
