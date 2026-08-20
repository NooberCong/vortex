import { injectScript } from "wxt/utils/inject-script";

import { post, type MseReport, type MseSignal } from "@/src/messages";

/**
 * The isolated-world half of channel 4.
 *
 * It exists because the hook has to run in the page's world, and an extension in the
 * page's world cannot talk to the extension. So: inject the hook as a `<script>` tag,
 * listen for what it posts back, and relay it.
 *
 * Registered dynamically, per origin, only after channel 3 has seen segments with no
 * manifest to explain them (`src/hook.ts`). It is never on `<all_urls>` — a MAIN-world
 * script on every page a user visits is the kind of blanket injection that gets an
 * extension removed, and it would be dishonest besides: almost no site needs this.
 *
 * `injectScript` is awaited so the tag is in the document before the page's own scripts
 * run, which is the only reason `document_start` was worth insisting on.
 */
export default defineUnlistedScript(async () => {
  window.addEventListener("message", (event) => {
    // `window.postMessage` is a channel anyone can write to. Same-window only, and the
    // shape is checked before anything is forwarded.
    if (event.source !== window) return;
    const signal = event.data as Partial<MseSignal> | null;
    if (!signal || signal.source !== "vortex-mse") return;

    const report: MseReport = {
      kind: "mse",
      signal: {
        source: "vortex-mse",
        codecs: Array.isArray(signal.codecs) ? signal.codecs.slice(0, 8) : [],
        urls: Array.isArray(signal.urls) ? signal.urls.slice(0, 24) : [],
        playing: signal.playing === true,
      },
    };
    // Only worth relaying once a buffer has actually been appended to. A page that
    // constructed a `MediaSource` and never used it is not playing anything.
    if (report.signal.playing) void post(report);
  });

  await injectScript("/mse-hook.js", { keepInDom: false });
});
