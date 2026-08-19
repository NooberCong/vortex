/**
 * Channel 4 — turning the MSE hook on, for one origin, only once it has earned it (03 §4).
 *
 * Some players fetch their manifest through a service worker, or build segment URLs in a
 * way `webRequest` cannot attribute to the visible video. The only place left to look is
 * inside the page, which means a MAIN-world script — and a MAIN-world script on every
 * page the user visits is exactly the kind of blanket injection that gets an extension
 * pulled. So this registers the loader per-origin, and only after channel 3 has already
 * seen segments arriving with no manifest to explain them.
 *
 * Registration rather than one-shot injection, because the hook has to patch
 * `MediaSource` *before* the player touches it, and only `document_start` is early
 * enough. A registered script is applied by the browser at the right moment on the next
 * load and survives worker eviction, which a one-shot cannot. The current page still gets
 * a one-shot as a consolation: `addSourceBuffer` has already been called by then, but
 * `appendBuffer` and `fetch` have not stopped, so it can still answer "is this playing".
 */

import { browser } from "wxt/browser";

const ENABLED_KEY = "mse:origins";
const SCRIPT_ID = "vortex-mse";
/** The isolated-world loader. It injects the MAIN-world script as a `<script>` tag. */
const LOADER = "/mse-loader.js";

/**
 * Turns the hook on for the origin serving `url`.
 *
 * Idempotent, and cheap to call again: the set of enabled origins lives in
 * `storage.local` so it survives both worker eviction and a browser restart, and a site
 * that needed the hook yesterday needs it today.
 */
export async function enable(url: string, tabId: number): Promise<void> {
  const origin = originOf(url);
  if (!origin) return;

  const enabled = await enabledOrigins();
  if (!enabled.includes(origin)) {
    enabled.push(origin);
    await browser.storage.local.set({ [ENABLED_KEY]: enabled });
    await register(enabled);
  }
  await injectNow(tabId);
}

/** Re-applies the registration after a browser restart. Called once from the background. */
export async function restore(): Promise<void> {
  const enabled = await enabledOrigins();
  if (enabled.length > 0) await register(enabled);
}

async function enabledOrigins(): Promise<string[]> {
  const stored = (await browser.storage.local.get(ENABLED_KEY))[ENABLED_KEY];
  return Array.isArray(stored) ? (stored as string[]) : [];
}

/**
 * One registration covering every enabled origin, replaced wholesale on each change.
 *
 * `updateContentScripts` would be the narrower call, but it fails when the id is not yet
 * registered and the first call is always that case. Unregister-then-register is one
 * branch instead of two and the list is a handful of entries.
 */
async function register(origins: string[]): Promise<void> {
  const scripting = browser.scripting;
  if (!scripting?.registerContentScripts) {
    // Firefox MV2 has `browser.contentScripts.register` instead, and it returns a handle
    // rather than taking an id. It is not wired up here because the MV2 target is a
    // Firefox build, and Firefox is also the browser where channel 3 rarely fails —
    // `webRequest` there sees service-worker traffic that Chrome hides. Recorded as a
    // gap rather than half-implemented.
    return;
  }
  try {
    await scripting.unregisterContentScripts({ ids: [SCRIPT_ID] });
  } catch {
    // Not registered yet, which is the normal first case.
  }
  try {
    await scripting.registerContentScripts([
      {
        id: SCRIPT_ID,
        matches: origins.map((origin) => `${origin}/*`),
        js: [LOADER],
        runAt: "document_start",
        allFrames: true,
        persistAcrossSessions: true,
      },
    ]);
  } catch {
    // A malformed origin, or a browser that refuses the pattern. The overlay still works
    // from channels 1–3; this one is the fallback, and a fallback that fails to install
    // must not take the rest down.
  }
}

/** The page that is already open does not get a `document_start`, so it gets this. */
async function injectNow(tabId: number): Promise<void> {
  try {
    await browser.scripting?.executeScript({
      target: { tabId, allFrames: true },
      files: [LOADER],
    });
  } catch {
    // The tab navigated away, or the build has no `scripting` permission.
  }
}

function originOf(url: string): string | null {
  try {
    const parsed = new URL(url);
    return /^https?:$/.test(parsed.protocol) ? parsed.origin : null;
  } catch {
    return null;
  }
}
