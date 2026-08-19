# 03 — The Capture Layer

The extension's job is to be a **sensor and a remote control**, never a downloader. It
observes, it hands off, and it renders the overlay. Every byte moves in `vortexd`.

---

## What Manifest V3 actually still allows

The widely repeated claim that MV3 killed download interception is wrong, and the confusion
costs projects months.

| Capability | MV3 (Chrome/Edge) | MV2 (Firefox) |
|---|---|---|
| **Observational `webRequest`** — `onBeforeRequest`, `onSendHeaders`, `onHeadersReceived`, `onCompleted` | ✅ fully intact | ✅ |
| `extraHeaders` → read `Cookie`, `Authorization`, `Origin`, `Referer` | ✅ | ✅ |
| **Blocking** `webRequest` / `webRequestBlocking` | ❌ policy-installed extensions only | ✅ Firefox kept it |
| `chrome.downloads.onCreated` / `onDeterminingFilename` / `cancel` | ✅ | ✅ |
| `chrome.cookies` incl. HttpOnly | ✅ | ✅ |
| Native messaging | ✅ | ✅ |

Vortex never needed to *block* a request — only to observe it and then cancel the resulting
download. That path is untouched by MV3, and it is the one every build relies on.

Blocking still buys one thing on Firefox that cancelling afterwards cannot: never starting
the download at all. See §2a.

**Firefox is the more capable target** and should be treated as the flagship for anything
experimental. WXT builds both from one source.

---

## The five capture channels

```
┌─ 1 ─ webRequest observer ────────── every request: URL, method, headers, MIME, size
├─ 2 ─ downloads.onCreated ────────── the browser decided this is a file → take it over
│      └ 2a, Firefox ─────────────── or take the response before a download exists
├─ 3 ─ manifest sniffer ───────────── .m3u8 / .mpd / media MIME → a video exists here
├─ 4 ─ MSE content-script hook ────── the fallback for players that hide their manifest
└─ 5 ─ the page itself ───────────── a player, and nothing on the wire explained it
```

The first four all watch the **network**, and they all rest on the same assumption: that a
page which plays video fetches something recognisable as video. Channel 5 is what happens
when that assumption is wrong.

### 1 — The request ledger

`webRequest` builds a rolling per-tab ledger of `RequestEnvelope`s:

```ts
interface RequestEnvelope {
  url: string; finalUrl: string; method: string;
  headers: Record<string,string>;      // via extraHeaders
  cookies: string;                     // chrome.cookies, incl. HttpOnly
  mimeType?: string; contentLength?: number;
  tabId: number; frameId: number;
  initiator?: string; referrer?: string;
  pageUrl: string; pageTitle: string;  // for naming
  capturedAt: number;
}
```

Bounded: 500 entries per tab, evicted LRU, cleared on tab close. This ledger is what makes
handoff work — the engine replays exactly what the browser sent.

### 2 — Download takeover

```
downloads.onCreated(item)
   │
   ├── is the origin on the DRM denylist?          → do nothing, let the browser handle it
   ├── is it below the size floor / a data: URL?   → do nothing
   ├── is vortexd reachable RIGHT NOW?             → if not, DO NOTHING
   ├── is it still in_progress?                    → if not, do nothing
   │
   ├── probe: does vortexd actually get bytes?     → if not, do nothing
   ├── downloads.cancel(id)
   ├── did the cancel take (state == interrupted)? → if not, DO NOTHING
   ├── downloads.erase(id)
   └── port.postMessage({ Submit: jobSpec })
```

**`DownloadItem` is a snapshot, and the two state checks are not paranoia.** `state` is
always `in_progress` at `onCreated`; a small file can be written to disk during the
filename grace and the probe. `downloads.cancel` on a download that already finished
*resolves successfully* — it is a no-op, not an error — so a `try`/`catch` around it
catches nothing, and the result is a file the browser saved, erased from the download list,
and then fetched a second time by Vortex. Only `downloads.search` says what actually
happened.

`onDeterminingFilename` is also registered: it fires before completion and yields the
post-redirect `finalUrl` and the browser's resolved filename — strictly better naming than
guessing from the URL. It must call `suggest()` or the download hangs forever.

**The non-negotiable rule: if the daemon is not reachable, do not cancel.** A download
manager that eats the user's download because its own service crashed is worse than no
download manager. The extension pings the port on startup and holds a live connection
state; when it is `Disconnected`, capture is fully passive.

### 2a — Pre-emption, on Firefox

Cancelling a download after the browser started it is the best MV3 allows, and it is
always a step late. Firefox kept blocking `webRequest` *and* is the only browser where a
blocking listener may answer with a promise — so the response can be suspended, decided on,
and cancelled before a `DownloadItem` exists at all:

```
onHeadersReceived(response)
   │
   ├── not a navigation, not a GET, not 200?       → let it through
   ├── not `Content-Disposition: attachment`?      → let it through
   │   (only from here is the response suspended)
   ├── denylisted / opted out / below the floor?   → let it through
   ├── probe: does vortexd actually get bytes?     → if not, let it through
   ├── 1.5 s budget spent waiting?                 → let it through
   └── { cancel: true } + Submit
```

Nothing is written, nothing appears in the download list, and there is no race to lose.
**Every branch that is not the last lets the response through**, where channel 2 sees an
ordinary download and gets its own turn — this channel can only improve on the one behind
it, never replace it.

Three things make it safe to run in front of a request rather than after one:

- **Navigations only, GET only, `attachment` only.** Channel 2 acts on the browser's own
  verdict that something is a file; this channel *predicts* it, and a wrong prediction
  cancels a request the browser would have rendered. `Content-Disposition: attachment` on a
  navigation is the one case that is never ambiguous — an `application/octet-stream` body or
  a PDF depends on browser settings this code cannot see, and a page `fetch`ing an
  attachment endpoint to build a blob is not downloading anything. GET, because the daemon
  re-issues the request and the browser's POST already reached the server.
- **A budget on the whole decision, not on one step.** What the user feels is the total: a
  click that appears to do nothing. A daemon that has not answered in 1.5 s is one they are
  better off downloading without. A port that disconnects fails every in-flight request
  immediately rather than at its timeout, so "vortexd is not running" costs milliseconds.
- **No `Ping`.** Channel 2 pings because its cancel is destructive and comes before the
  probe; here the probe is the only thing that can trigger a cancel, and a probe the daemon
  answered is proof of life strictly stronger than a ping. One round trip, not two, on the
  path where someone is waiting.

`webRequestBlocking` is requested in the MV2 manifest only. Chrome would not honour it
outside enterprise policy, and asking for a permission the code cannot use is a review flag.
`BLOCKING_WEBREQUEST` folds to a literal at build time, so the Chrome bundle does not
contain this path at all.

### 3 — Manifest sniffing

`onBeforeRequest` + `onHeadersReceived` flag:

- `.m3u8`, `.mpd` by path or by `Content-Type`
  (`application/vnd.apple.mpegurl`, `application/x-mpegURL`, `application/dash+xml`)
- direct media: `video/mp4`, `video/webm`, `audio/*` above a size floor
- `.ts`, `.m4s`, `.cmfv` segment bursts — a strong signal the manifest was missed

The envelope goes to `vortexd` via `ProbeMedia`, which parses the manifest and returns a
`MediaCandidate[]` ladder. The overlay renders that.

### 4 — MSE hook (fallback)

Some players fetch a manifest through a service worker, or construct segment URLs in a way
`webRequest` can't attribute to the visible video. A content script injected at
`document_start` in the **MAIN world** patches:

```js
MediaSource.prototype.addSourceBuffer   // → codec strings, which the ladder needs
SourceBuffer.prototype.appendBuffer     // → init segment, byte volume, real playback state
window.fetch / XMLHttpRequest           // → segment URLs the network layer couldn't attribute
```

It reports metadata only — never bytes. Its real value is answering "is a video *actually
playing* right now, and which one" so the overlay can be right instead of eager.

Injected only on pages where channel 3 already saw a media signal. No blanket main-world
injection.

---

### 5 — The page itself (last resort)

The four channels above watch the wire. A few of the largest sites on the web give the wire
nothing to see, and YouTube is the worked example — every pattern misses, in a different way:

| what channel 3 looks for | what YouTube does |
|---|---|
| `.m3u8` / `.mpd` by path | ships neither; `streamingData` is JSON inside the document |
| a manifest `Content-Type` | the document is `text/html` |
| a `video/*` body over the size floor | ranges of ~3 MB, under the 4 MB floor |
| `.ts` / `.m4s` segment paths | `videoplayback?…`, no extension at all |

The last row is the one that hurts most: the segment burst is what *enables* channel 4, so a
site whose segment URLs have no extension does not get the fallback either. Four channels,
four misses, no overlay, and nothing anywhere saying why.

So when the page has a real player and none of them has produced a ladder, the content
script offers the **page URL** to the daemon, which hands it to the extractor
(04 §yt-dlp). It is deliberately last, and deliberately narrow — the cost of asking is a
subprocess, and the thing being sent is the URL the user is looking at:

- A **real player**, by the same rules that decide where a badge goes: big enough, painted,
  not a sound effect, not a decorative background loop.
- **With something in it** — metadata and a finite duration. Not *playing*: someone who
  opens a page and reads the description before pressing play still wants the button.
- **After 3 seconds**, so the faster channels get their turn. An extractor run in front of
  an ordinary HLS site's own probe is a subprocess spent on a page that was about to answer
  for itself.
- **Once per page**, re-armed on a route change — on a single-page app the content script is
  never reloaded and the second video is a different video.
- Behind the same capture switch, denylist and per-origin opt-out as every other channel,
  and the denylist is checked against the page URL directly rather than through a tab
  lookup, because here the URL being sent *is* the page.

Absent from the store build in the same way channels 3 and 4 are: not disabled, not present.

---

## Handoff: the hard part

Re-issuing a browser request from another process fails when the URL is single-use,
session-bound, or fingerprint-gated. Mitigations, in order:

1. **Replay the envelope verbatim** — same UA, `Referer`, `Origin`, `Sec-Fetch-*`, full
   cookie jar. Most failures are a missing `Referer`.
2. **Probe before committing.** The daemon issues its `Range: bytes=0-0` probe *before* the
   extension erases the browser's download record. A non-2xx means abort the takeover and
   let the browser proceed. The user sees a normal download, not a failure.
3. **URL renewal.** When a URL expires mid-transfer, `UrlExpired` goes back to the
   extension, which re-acquires a fresh one in page context (re-request the originating
   endpoint from the tab, or reload a hidden frame of the origin page) and returns
   `RenewedUrl`. The engine resumes on the existing bitmap. See 02 §6.
4. **Give up gracefully.** If renewal fails, the job pauses as resumable with a
   "Reopen the page to continue" action — never a dead-end error.

---

## Service worker lifetime (MV3)

The MV3 service worker is evicted after ~30 s idle. Non-negotiable rules:

- **Register every listener at top level, synchronously.** A listener registered inside an
  async callback is silently lost on the next wake.
- **Zero in-memory state.** The request ledger and pending takeovers live in
  `chrome.storage.session`.
- **Reconnect the native port lazily.** An open `connectNative` port extends the worker's
  life, but do not depend on that — treat every wake as cold and re-establish.
- Keep an alarm (`chrome.alarms`, 1 min) as a liveness floor for reconnect-on-failure only —
  not as a keepalive hack.

---

## The overlay (video)

Injected in a **closed shadow root** so page CSS cannot reach it and page scripts cannot
read it. The host is a viewport-sized layer that catches no clicks; what is in it is
positioned per player.

**Collapsed** — a badge in the player's own top-right corner, 10 px inset, appearing only
when a real, downloadable stream is confirmed. One badge per player, not one per page:

```
 ┌───────────────────────────────────────────────┐
 │                        ╭────────────────────╮ │
 │                        │ ↓ Download · 1080p │ │
 │                        ╰────────────────────╯ │
 │                                               │
 │                  ▶                            │
 │                                               │
 │  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━   │
 └───────────────────────────────────────────────┘
```

Which ladder belongs to which player is decided in the page, because `webRequest` sees a
manifest URL and a tab id and no element (`src/overlay/anchor.ts`):

1. **Duration.** A player and a manifest that agree to within 2 % are the same stream.
   Nothing else on a page comes that close by accident — an ad break, a trailer and a
   preview roll are all manifests on the same page as the feature, and all a different
   length.
2. **Size order**, for whatever is left: biggest player, longest stream. This is the rule
   that carries the ordinary page — one player, one manifest, `video.duration` still `NaN`
   because the metadata has not landed.

Both rules take the page at its word about what a player is, so the filtering in front of
them matters as much as the rules do. A `<video>` at `opacity: 0` waiting to preload, one
parked at `left: -9999px`, one holding a `data:audio/` sound effect: all full-sized, all
connected, and any of them will take the badge that belonged to the feature. A **full-bleed
background loop** is worse, because it is usually the *largest* `<video>` on the page and
the size rule rewards exactly that — so it sorts behind everything real. Decoration is
identified by `object-fit: cover` **that is actually cropping**: the property alone is not
the signal it looks like, since YouTube's own watch player computes to `cover` and crops
nothing by it, while a 16:9 source poured into a hero band is cropped by more than half.

A manifest that is only a *rung* of another manifest is dropped before any of this. The
sniffer cannot tell a master playlist from one of its own media playlists — both are
`.m3u8` and both go past on the wire — and a rung comes back as one variant with no
resolution and no bitrate, carrying the master's duration. That makes duration pairing a
coin toss whose loser is a five-rung ladder rendering as a single line reading `0 kbps`.
No heuristic is needed to spot it: in HLS a master names its rungs by URL, so a candidate
whose manifest *is* one of another candidate's variant ids is that variant, seen twice.

Spare manifests are left unpaired on purpose. When *nothing* pairs — an audio-only stream,
a player this code cannot see — the overlay falls back to a single pill in the
bottom-right of the viewport, 24 px inset, which is the one thing it can always do.

**Expanded** — the ladder, on click. Every rung is a button:

```
 ╭─────────────────────────────────────────────╮
 │  Session 3 — Distributed Consensus          │
 │  HLS · 42:17                                │
 │ ─────────────────────────────────────────── │
 │  Subtitles   English, Spanish     [ ✓ ]     │
 │ ─────────────────────────────────────────── │
 │     2160p   HEVC              ~3.8 GB       │
 │     1080p   H.264             ~1.2 GB       │
 │                              PLAYING        │
 │      720p   H.264             ~680 MB       │
 │     Audio only                 ~82 MB       │
 │ ─────────────────────────────────────────── │
 │  Video and audio will be combined.          │
 │                        Not on this site     │
 ╰─────────────────────────────────────────────╯
```

The panel closes on its own × , on Escape, and on a click anywhere outside it — and
Escape is only swallowed when there was a panel to close, because Escape belongs to the
page and to the browser. Clicking the badge again is not one of the ways out: the badge is
what the panel replaced.

Rules that keep it from being obnoxious:

- Appears only on **confirmed** streams — manifest parsed, a variant is actually reachable.
  Never on a hunch.
- **Choosing a resolution is the download.** A rung is not selected and then confirmed;
  there is no second button to press. The rung matching the current playback resolution is
  marked `PLAYING`, and it is marked, not preselected — a mark is information, and the
  extra click a preselection saves is the click that made the choice invisible.
- The marked rung is never the maximum. People downloading a lecture do not want 4 GB.
- Subtitles sit **above** the ladder, because a rung is the last click: whatever is set
  when it is pressed is what gets downloaded.
- Quiet by default. A badge introduces itself for 4 s when it appears and then waits for
  the pointer to come back to the player — so a page is not permanently wearing a button.
  It stays reachable by Tab the whole time (`opacity`, never `visibility`), and stays up on
  a device with no hover at all.
- Dismissible per-origin, persistently, from the overlay itself.
- Never covers player controls: it sits in the corner opposite them, and in fullscreen the
  host moves **into** the fullscreen element — the top layer paints that subtree and
  nothing else, so an overlay parented to `<html>` is simply not on the screen, whatever
  its `z-index`.
- **Never floats over a page pointing at nothing.** Where the player *is* and where a badge
  pinned to it can be *seen* are different questions, and `src/overlay/geometry.ts` answers
  the second one:
  - **Clipping ancestors.** A player in a carousel or a collapsing card has a rect that runs
    well past the box that shows it, and the corner the badge is pinned to is routinely the
    corner that is gone. The rect is cut down to the intersection — but only by ancestors
    that genuinely clip it: a player with `position: absolute`, which is how nearly every
    custom player fills its container, is clipped by its containing block and by nothing
    between the two, and inventing clipping the browser does not apply hides badges for a
    reason that is not real.
  - **A sticky header.** Measured by hit-testing the top edge of the window, because sites
    agree on nothing about what a header is called and completely about it being the thing
    painted at the top. The element that *pins* the bar is often not the element that
    *paints* it — YouTube's masthead is a fixed box with a transparent background and an
    absolutely positioned child carrying the colour — so an opaque, full-width, short box
    with any pinned ancestor counts. The badge comes down below it.
  - **Cover.** A modal, a consent wall, a sticky player from another article: the player is
    exactly where it was and nobody can see it. Judged on `background-color` alone, because
    every player on the web layers transparent scrims and controls over its own video and
    counting those as cover would hide the badge on the sites it exists for.

  None of the three has an event and all are far too slow for sixty frames a second, so
  they are measured on the same half-second repair tick that re-pairs players — measured
  *before* the frame is placed, hit-tested *after*, so a badge that has just moved out from
  under a header does not stay hidden until the next one.
- Muxing note is honest — "Video and audio will be combined" — because that step takes time
  and users should know why the job has a second phase.

**Non-video downloads do not get an overlay.** They get the desktop New Download sheet
(05 §Screens), because that is where a save path, a name, and a category belong.

---

## The DRM denylist

Enforced in **two independent places**, because one is a policy and the other is a
guarantee.

**In the extension** — the overlay never renders, the sniffer never reports, on:

```
netflix.com          disneyplus.com       primevideo.com
max.com              hbomax.com           hulu.com
tv.apple.com         peacocktv.com        paramountplus.com
crunchyroll.com      spotify.com          music.youtube.com
… plus their regional domains and CDN hosts
```

**In the engine** — refuse regardless of origin when a manifest carries:

- HLS: `#EXT-X-KEY:METHOD=SAMPLE-AES` with a DRM `KEYFORMAT`
  (`com.apple.streamingkeydelivery`, `urn:uuid:edef8ba9-…` Widevine, `com.microsoft.playready`)
- HLS: `#EXT-X-SESSION-KEY` with any DRM key format
- DASH: any `<ContentProtection>` with a Widevine / PlayReady / FairPlay `schemeIdUri`
- Any response indicating an EME/CDM license exchange

The user-facing message is one sentence, no lecture, no workaround hint:

> **Protected content.** Vortex doesn't download DRM-protected video.

See 04 §DRM for why this is a product boundary and not a limitation to route around.

---

## Store strategy

Chrome Web Store Developer Program Policy: *"Do not encourage, facilitate, or enable the
unauthorized access, download, or streaming of copyrighted content or media."* YouTube
downloading specifically results in rejection, removal, or developer-account termination.
The 2025–2026 enforcement waves cleared out most of the category.

Therefore:

| | |
|---|---|
| **Listing** | "Vortex — download manager." Files, speed, resume, queue. No streaming-site claims, no logos, no site names in the description, no video-downloading screenshots. |
| **Store build** | Generic HTTP capture only. Site-specific extractors ship in `vortexd`, which the store does not govern — exactly the structure IDM, FDM and XDM use. |
| **Firefox / AMO** | The flagship. More permissive review, and blocking `webRequest` is still available. |
| **Edge Add-ons** | Mirrors Chrome policy. Same conservative build. |
| **Self-hosting for Chrome** | Not viable — Chrome blocks off-store CRX installs on stable. The store is mandatory for Chrome users, so the store build must stay clean. |

Assume the Chrome listing can be pulled at any time regardless of compliance. The product
must remain usable without it: `vortexd` has clipboard monitoring and a "paste URL" path,
and Firefox remains fully functional. **Do not architect anything that only works if the
Chrome extension survives.**
