import { defineConfig } from "wxt";

import { EXTENSION_KEY } from "./identity";

/**
 * Two targets from one source (03 §What Manifest V3 actually still allows).
 *
 * - **Chrome/Edge → MV3.** Observational `webRequest` is intact there; only *blocking*
 *   `webRequest` went away, and Vortex never needed to block. It observes, then cancels
 *   the download the browser created.
 * - **Firefox → MV2.** The flagship, and the more capable target. Anything experimental
 *   should land here first.
 *
 * `--mode store` produces the conservative build described in 03 §Store strategy: generic
 * HTTP capture, nothing about streaming. See `src/build.ts`.
 */
export default defineConfig({
  srcDir: ".",
  outDir: ".output",

  manifest: ({ browser, manifestVersion, mode }) => {
    const store = mode === "store";
    const permissions = [
      // Observational everywhere; MV2 adds `webRequestBlocking` below, where the platform
      // still allows a response to be held and cancelled before it becomes a download.
      "webRequest",
      "downloads",
      "cookies",
      "nativeMessaging",
      "storage",
      "alarms",
      "tabs",
    ];
    // The MSE hook is registered per-origin at runtime, which needs `scripting`. The
    // store build has no hook, so it does not ask.
    if (!store && manifestVersion === 3) permissions.push("scripting");

    return {
      name: "Vortex",
      short_name: "Vortex",
      // No streaming-site claims, no site names, no logos. The listing is a download
      // manager, because that is what the store's policy permits it to be.
      description:
        "Download manager. Parallel transfers, real resume, and a queue that survives a restart.",
      homepage_url: "https://vortex.download",

      permissions,
      host_permissions: manifestVersion === 3 ? ["<all_urls>"] : undefined,
      // MV2 has no `host_permissions`; the host list goes in `permissions`.
      //
      // `webRequestBlocking` is MV2-only in both directions: Chrome restricts it to
      // policy-installed extensions, and Firefox kept it. It buys the one thing cancelling
      // a download afterwards cannot — never starting it — so `src/intercept.ts` takes a
      // response before a `DownloadItem` exists. Chrome does not ask for what it cannot
      // use, and `tests/store-build.test.ts` holds that line.
      ...(manifestVersion === 2
        ? { permissions: [...permissions, "webRequestBlocking", "<all_urls>"] }
        : {}),

      // The extension id has to be stable: the native messaging host manifest pins
      // `allowed_origins`/`allowed_extensions` to exact ids, and the platform forbids
      // wildcards there (01 §Security boundaries).
      //
      // Chromium derives the id from `key`, and without one it falls back to a hash of the
      // folder the unpacked build was loaded from — different on every machine, so nothing
      // could be registered in advance. Declaring the key fixes the id at
      // `identity.ts`'s `extensionId()`, which is compiled into
      // `vortex_setup::CHROMIUM_IDS`.
      //
      // Not in the store package. There the id belongs to the listing, assigned when the
      // item is created, and shipping a key that claims a different one is at best
      // ignored and at worst rejected. `tests/store-build.test.ts` holds that line.
      ...(browser !== "firefox" && !store ? { key: EXTENSION_KEY } : {}),

      ...(browser === "firefox"
        ? {
            browser_specific_settings: {
              gecko: {
                id: "vortex@vortex.download",
                strict_min_version: "115.0",
                // Required for new listings from November 2025, and true: the extension
                // sends nothing anywhere. Every byte it observes goes to a daemon on the
                // same machine, over a pipe only this user can open.
                data_collection_permissions: { required: ["none"] },
              },
            },
          }
        : {}),

      // Only what the page has to be able to load: the MAIN-world hook, and only where
      // the loader was registered.
      web_accessible_resources: store
        ? undefined
        : [
            manifestVersion === 3
              ? { resources: ["mse-hook.js"], matches: ["<all_urls>"] }
              : ("mse-hook.js" as never),
          ],
    };
  },

  hooks: {
    /**
     * The store build must not merely disable the MSE hook — it must not contain it.
     * `MEDIA_CAPTURE` tree-shakes the code that *calls* it, but the two MAIN-world
     * entrypoints are roots of their own and would still be emitted, sitting in the
     * package patching `MediaSource` with nothing to reach them. A reviewer reads files,
     * not call graphs.
     */
    "entrypoints:resolved": (wxt, entrypoints) => {
      if (wxt.config.mode !== "store") return;
      for (let i = entrypoints.length - 1; i >= 0; i--) {
        if (entrypoints[i]!.name.startsWith("mse-")) entrypoints.splice(i, 1);
      }
    },
  },

  vite: () => ({
    build: {
      // A capture layer that ships its own source map ships its heuristics with it.
      sourcemap: false,
      target: "es2022",
    },
  }),
});
