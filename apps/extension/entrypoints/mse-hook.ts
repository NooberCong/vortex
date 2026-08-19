/**
 * Channel 4 — the MSE hook (03 §4). Runs in the page's own world.
 *
 * Some players fetch their manifest through a service worker, or build segment URLs from
 * pieces `webRequest` cannot attribute to the visible video. Inside the page there is no
 * such ambiguity: `MediaSource` is where every one of them ends up.
 *
 * **It reports metadata only, never bytes.** Codec strings, because the ladder needs them
 * and the manifest may be hidden; URLs that look like segments, because the network layer
 * could not attribute them; and one boolean saying whether anything has actually been
 * appended. That last one is the real value — it answers "is a video *playing* right now",
 * which is the difference between an overlay that is right and one that is eager.
 *
 * Three rules for patching someone else's page, all of which are about being unnoticeable:
 * every patch calls through, every patch is wrapped so a throw here cannot break the
 * player, and nothing is added to any object the page can enumerate.
 */
export default defineUnlistedScript(() => {
  const REPORT_DELAY = 400;
  const MAX_URLS = 24;

  const codecs = new Set<string>();
  const urls = new Set<string>();
  let playing = false;
  let scheduled: ReturnType<typeof setTimeout> | null = null;

  const SEGMENT = /\.(m3u8|mpd|ts|m4s|cmfv|cmfa|mp4|webm)(\?|#|$)/i;

  const report = () => {
    scheduled = null;
    window.postMessage(
      {
        source: "vortex-mse",
        codecs: [...codecs],
        urls: [...urls],
        playing,
      },
      "*",
    );
  };

  const schedule = () => {
    // A player calls `appendBuffer` several times a second. Coalescing means the page
    // posts a message every 400 ms at worst, whatever the segment rate is.
    if (scheduled === null) scheduled = setTimeout(report, REPORT_DELAY);
  };

  const noteUrl = (value: unknown) => {
    if (typeof value !== "string" || !SEGMENT.test(value)) return;
    if (urls.size >= MAX_URLS) return;
    try {
      urls.add(new URL(value, location.href).href);
      schedule();
    } catch {
      // A relative URL against an opaque base. Not worth reporting.
    }
  };

  /** Replaces `target[name]`, calling through and swallowing anything our side throws. */
  const patch = <T extends object>(
    target: T,
    name: keyof T & string,
    observe: (args: unknown[]) => void,
  ) => {
    const original = target[name];
    if (typeof original !== "function") return;
    const replacement = function (this: unknown, ...args: unknown[]) {
      try {
        observe(args);
      } catch {
        // The page's player must not notice that anything is watching.
      }
      return (original as (...a: unknown[]) => unknown).apply(this, args);
    };
    // Keep `fn.length` and `fn.name` intact: players fingerprint their own environment,
    // and a patched method that reports the wrong arity is a detectable change.
    Object.defineProperty(replacement, "name", { value: original.name });
    Object.defineProperty(replacement, "length", { value: original.length });
    target[name] = replacement as T[keyof T & string];
  };

  if (typeof MediaSource !== "undefined") {
    patch(MediaSource.prototype, "addSourceBuffer", (args) => {
      const type = args[0];
      if (typeof type === "string") {
        codecs.add(type);
        schedule();
      }
    });
  }

  if (typeof SourceBuffer !== "undefined") {
    patch(SourceBuffer.prototype, "appendBuffer", () => {
      // Nothing about the buffer itself is read. Its existence is the whole signal.
      playing = true;
      schedule();
    });
  }

  patch(window, "fetch", (args) => {
    const input = args[0];
    noteUrl(typeof input === "string" ? input : (input as Request | undefined)?.url);
  });

  if (typeof XMLHttpRequest !== "undefined") {
    patch(XMLHttpRequest.prototype, "open", (args) => noteUrl(args[1]));
  }
});
