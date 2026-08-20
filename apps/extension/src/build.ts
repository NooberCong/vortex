/**
 * Which build this is.
 *
 * The Chrome Web Store's Developer Program Policy forbids "enabling the unauthorized
 * access, download, or streaming of copyrighted content or media", and the 2025–2026
 * enforcement waves cleared out most of the category (03 §Store strategy). So the store
 * build ships generic HTTP capture only: no manifest sniffing, no MSE hook, no overlay.
 * Everything it drops lives in `vortexd`, which the store does not govern — the same
 * structure IDM, FDM and XDM use.
 *
 * `import.meta.env.MODE` is replaced at build time, so this folds to a literal and the
 * media code is genuinely absent from the store bundle rather than merely unreachable.
 * That distinction matters: a reviewer reads the bundle, not the intent.
 */
export const MEDIA_CAPTURE = import.meta.env.MODE !== "store";

/**
 * Can this build hold a response open while it decides?
 *
 * MV3 is the one place blocking `webRequest` was removed, so the constant is written as
 * "not MV3" rather than "Firefox": it is the capability that matters, and phrasing it
 * that way also makes it true under the test runner, which has no build env to read.
 * Like `MEDIA_CAPTURE` it folds to a literal, so the Chrome bundle does not contain the
 * pre-emption path at all.
 */
export const BLOCKING_WEBREQUEST = import.meta.env.MANIFEST_VERSION !== 3;

/** The native messaging host. Its manifest pins `allowed_origins` to exact extension ids. */
export const HOST_NAME = "io.vortex.host";

/** How this client introduces itself in `Hello`. */
export const CLIENT_NAME = "vortex-extension";

/**
 * Where the app comes from.
 *
 * The extension is half of Vortex and the smaller half: it watches, and it asks. Every
 * byte is fetched by `vortexd`, which is installed separately — so on a machine where the
 * app was never installed, nothing the extension offers can work, and the user is one
 * click away from fixing it if anyone tells them where to click. Nobody was telling them.
 *
 * `/releases/latest` rather than a pinned tag: a version in here would go stale on the
 * next release and send people to an old installer, and GitHub already redirects this to
 * whatever the newest one is.
 */
export const RELEASES = "https://github.com/NooberCong/vortex/releases/latest";
