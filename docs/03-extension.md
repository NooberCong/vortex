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
       └ 5a ─ frame reporter ─────── …including a player the top document cannot see
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

Bounded twice over, because the count is not the thing that runs out: 500 entries per tab
**and** 256 KB per tab, whichever bites first, evicted oldest-first and cleared on tab
close. A remembered POST body (capped at 64 KB — above that it is a file upload, not a
request that becomes a download) is orders of magnitude larger than a header list, and the
session area is one pool shared with every other tab. This ledger is what makes handoff
work — the engine replays exactly what the browser sent.

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

### 2b — The acknowledgement

Both channels above end by taking the browser's own record of the download away: channel 2
cancels the `DownloadItem` and then **erases** it, and pre-emption cancels the response
before one is ever created. That is what makes the handoff clean, and it is also the worst
thing about it, because of what the user sees:

```
click a link  →  the download bubble flickers  →  nothing, anywhere
```

There is no interrupted row to inspect and no entry in the browser's list, so the honest
reading of that sequence is *the download failed*. The honest response to a download that
failed is to click the link again — which runs the whole handoff a second time and produces
a duplicate job. **Silence here is not a missing nicety; it is a bug that costs the user
bandwidth.**

So a takeover says three things, in descending order of how reliably it can say them.

**The toolbar badge** (`src/toolbar.ts`, `src/queue.ts`) is the floor, and the only one of the
three that always works. Both browsers put their own download indicator in the toolbar a few
pixels from the extensions area; that is where a decade has taught people to look after
clicking a link, and it is the indicator Vortex just took away. It needs no permission, it
survives an evicted worker, and it is there for the cases the other two cannot reach: a
download started from a PDF viewer, from a `file://` page, from a tab that closed behind
itself, or from an origin the content script is excluded from.

It counts **unfinished** jobs, not moving ones — a paused download is still one the user is
expecting — and it counts what the *daemon* says it has rather than what this extension
submitted, so a handoff that reached `Submit` and then failed inside the daemon leaves no
phantom on the toolbar. That is fed by the structural events (`JobAdded`,
`JobStateChanged`, `JobFinished`, `JobRemoved`) which the daemon broadcasts to every client
regardless of subscription, reconciled against a full `List` whenever the count could have
drifted: a fresh worker, the liveness alarm, an opened popup. **There is deliberately no
`Subscribe`.** `Summary` scope delivers a frame per job at 2 Hz, which would keep the MV3
service worker permanently awake to animate a number nobody is looking at.

It is achromatic. `--attention` is the only colour the tokens allow outside the segment map
and it means something is wrong; a download starting is not wrong (05 §The one rule). The
signal is that a badge is on an icon that normally carries none.

**The in-page receipt** (`src/receipt/`) is the one that names the file, which is the part
that actually answers the user's question — "a download started" is not news, "*that*
download started" is. A small card in the bottom-right of the page the click came from, in
a closed shadow root, for four seconds:

```
                                   ┌──────────────────────────────┐
                                   │ ↓  Downloading in Vortex   › │
                                   │    ubuntu-24.04.2-live.iso   │
                                   └──────────────────────────────┘
```

Five properties, each of them a decision:

- **The whole card is one button, and it opens the popup.** A card that announces a file and
  then cannot be followed is a dead end; the popup is where that same file is a row with a
  bar under it, and it is the panel the user would otherwise have to go and find. Not a
  small *View* target in the corner — a card that is mostly not the control it looks like is
  worse than one that is entirely the control it looks like. The `›` carries the affordance
  statically, because nobody hovers something they have been told is about to leave in order
  to discover whether it is a button.

  It cannot open a popup itself: `action.openPopup` is an extension API and a content script
  is not the extension. The press goes out as `{ kind: "popup" }` and the background calls it
  (`src/toolbar.ts`). **That API is young enough to be missing on browsers Vortex supports** —
  Chrome 127+, Firefox 118+, against a `strict_min_version` of 115 — and a content-script
  message does not carry user activation, which older Firefox demanded. So `toolbar.open()`
  returns a boolean rather than throwing, and where it says `false` the background asks for
  `Reveal { job: null }` instead. That is a bigger gesture than was asked for and it shows
  the same transfers, which is the right way round: a click that lands on nothing is exactly
  the failure this card exists to prevent.
- **It cannot swallow the click that summoned it.** Inert for 500 ms after it appears —
  longer than the fade-in — because it arrives unasked in the corner of a page someone is
  using, a few hundred milliseconds after they clicked something, and a control that
  materialises under a moving cursor can take a click meant for the page behind it. Nobody
  aims at something that was not there half a second ago, so a click inside that window was
  already on its way somewhere else and should land there. The host element stays
  `pointer-events: none !important` regardless; only the card arms.
- **It counts rather than stacks.** Five links clicked in five seconds are one card saying
  *5 downloads in Vortex*, not five cards. A batch handover is exactly what a download
  manager is for. Each handover re-arms the guard, because each one was a separate click.
- **It leaves by itself**, with no dismiss button, because a control implies a decision and
  there is none here — but it waits under the cursor. A card that faded out from under
  somebody reaching for it would be a control that punishes being used.
- **It is sent by `handoff.hand`, from both channels.** A receipt shown on Chrome and not on
  Firefox is the same class of divergence the rest of that module exists to prevent — one
  that survives review because both halves look correct in isolation.

It is **out of the page's tab order** (`tabindex="-1"`). A control that inserts itself into
someone's tabbing unannounced and then disappears four seconds later is a worse citizen than
one that cannot be tabbed to, and the keyboard-reachable route to the same panel — the
toolbar button — is what this card is a shortcut for rather than a replacement of.

The name is derived locally (the browser's resolved filename, else the `Content-Disposition`
hint, else the URL leaf) rather than waited for from `JobAdded`. The daemon derives a better
one (04 §8), but the receipt's whole value is being on screen *before* the user has decided
the click failed.

**The popup's transfer list** is the third, and it is where a handover can be found by name
a minute later. See §The popup.

### 2c — `Reveal`

A row in that list is clickable, and so is the *Open Vortex* button under it. Neither can do
anything directly: the extension lives in a browser, the window lives in `vortex-app`, and
the only thing the two share is the daemon. So the click goes out as
`Reveal { job: Option<JobId> }`, and the daemon starts `vortex-app --reveal <id>` beside
itself.

That one path covers both cases without the daemon having to tell them apart — a question it
could not answer anyway, since the app connects over the same endpoint every other client
does. If no window is open, one opens. If one is already open, the app's single-instance
plugin hands the arguments to it and the second process exits without drawing anything.

The window then does what only the list can: move the filter if the job is not under the
current one, select the row, and expand it. A **cold** start cannot be told by an event —
the arguments are parsed before the webview exists — so the frontend reads them as state on
mount, exactly as it reads `connected()`, and holds the id until the job actually turns up
in a list that is still one round trip away.

### 3 — Manifest sniffing

`onBeforeRequest` + `onHeadersReceived` flag:

- `.m3u8`, `.mpd` by path or by `Content-Type`
  (`application/vnd.apple.mpegurl`, `application/x-mpegURL`, `application/dash+xml`)
- direct media: `video/mp4`, `video/webm`, `audio/*` above a size floor
- `.ts`, `.m4s`, `.cmfv` segment bursts — a strong signal the manifest was missed

The envelope goes to `vortexd` via `ProbeMedia`, which parses the manifest and returns a
`MediaCandidate[]` ladder. The overlay renders that.

One manifest URL is seen dozens of times — a playlist is re-fetched every few seconds — so
the sniffer remembers what it has asked about. There is no error path back from
`ProbeMedia`: the daemon answers with a ladder or it does not answer at all. So a URL that
produced a ladder is pinned and never probed again, and one that produced nothing is held
for 30 seconds and then allowed to be asked about afresh. A probe can go nowhere for reasons
that say nothing about the URL — the daemon was restarting, the port was dead until the next
liveness alarm, the origin refused a replayed envelope — and retiring the URL on that basis
turns a transient failure into a permanent silent one, on a manifest the player is still
happily fetching.

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

### 5a — The frame reporter

`entrypoints/content.ts` is `allFrames: false`, for two reasons that are both still right:
the overlay belongs to the page rather than to each of its twenty tracking frames, and
renewal works from the top document whichever frame owns the player, because cookies are
sent for the *target* origin.

The orphan watcher was swept along with them, and it should not have been. An enormous share
of the web's players sit in a cross-origin `<iframe>` — every `/embed/` URL, every
third-party player host — and from the top document those are not merely hard to find but
**unreachable**: `contentDocument` is `null` across origins, so `document.querySelectorAll("video")`
returns an empty list for as long as the page is open. Channel 5 could never fire on any of
them, which is to say the channel written for "all four network channels missed" had a blind
spot shaped like the most common way to embed a video.

So `entrypoints/frame.content.ts` is a second content script, `allFrames: true`, that does
exactly one thing: run the same `Orphans` watcher and report *there is a real player in
here*. It draws nothing, answers no messages, and reads the DOM without touching it.

The signal deliberately carries **no URL**. The top document is entitled to name itself
because it *is* the page; a subframe is routinely someone else's code, and a message that
let it nominate a URL would let it choose what the extractor goes and fetches. The
background takes the page from `sender.tab.url`, which the browser fills in and the frame
cannot influence, then runs it through the identical `probePage` guards as channel 5 proper.

Two consequences of reusing `Orphans` unchanged, both accepted rather than overlooked:
nothing calls `attributed()` in a frame, so a frame whose stream *was* attributed still asks
once and the background drops it on the "ladder already known for this tab" check; and a
player in an `about:srcdoc` or `blob:` frame is not reported, because the watcher declines
to ask from a document the daemon could not fetch and judges that by its own protocol.

Dropped from the store build as a file, like the MSE entrypoints — an unexplained
`all_frames` injection on `<all_urls>` is exactly what a reviewer is looking for.

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
- **The session area is finite and shared.** ~10 MB for the whole extension, while every
  bound above it is per tab. `src/session.ts` owns that fact: a write that overflows drops
  the largest cached keys and retries, never the key being written and never the small ones
  (the settings mirror, the badge's job ids). Everything large in there is a cache, so the
  cost of a reclaim is a probe, never a download. Nothing writes the area directly.
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
 │                   ╭────────────────────╮ ╭─╮ │
 │                   │ ↓ Download · 1080p │ │×│ │
 │                   ╰────────────────────╯ ╰─╯ │
 │                                               │
 │                  ▶                            │
 │                                               │
 │  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━   │
 └───────────────────────────────────────────────┘
```

The `×` on the end takes the badge off **this video until the page is reloaded**. It is a
sibling of the badge and not a control inside it, because a button nested in a button is
not something a browser renders — but the two are **one pill**, divided by a hairline: the
border, radius and surface belong to the row, not to either button. Placement measures that
row, which is what keeps the whole control inside the player rather than half over the page.

The cross is drawn by the stylesheet rather than typed. `×` (U+00D7) carries all its ink
above the baseline, so centring its box leaves the glyph about 1.5 px low, and the
correction would be a number true of one font — this shadow root falls back to `system-ui`
whenever the webfont has not loaded. Two rotated bars have no metrics to be wrong about.

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
- **Two ways out, and they are not the same size.** The `×` on the badge clears it for one
  video until the page is reloaded, in memory, telling the daemon nothing. *Not on this
  site* is per-origin, persistent, and reaches the daemon. Most of the time what someone
  wants is the first one — the badge is in the way *right now* — and an overlay that offers
  only the permanent opt-out collects opt-outs it did not deserve.
- The `×` is keyed by the candidate, not by the player. On a single-page app the `<video>`
  outlives what plays in it, so keying on the element would turn "hide this one" into
  "silence this player for everything it shows next".
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

## The popup

The toolbar panel, and the only surface in the extension that exists in every build — the
store package has no overlay at all, and this is what it has instead.

It exists because the overlay could not be the only way in. *Not on this site* removes the
overlay, which made it a one-way door: from that moment every capture path returned silently
for that origin, and the only control that could have undone it was the thing that had just
deleted itself. Nothing on the page said why.

Four sections, in the order somebody actually asks the questions:

1. **Is Vortex working at all?** The daemon light — `ready`, `stopped` or `missing`, never a
   boolean, because "start the app" and "you do not have the app" are different sentences
   with different buttons under them. `missing` is only ever asserted on evidence (a native
   host manifest the platform says is not there), and it is the one case that gets a *Get
   Vortex* button: the extension installs on its own, and on its own it does nothing.
2. **What is it doing right now?** The transfers, below.
3. **Is it switched on here?** The site switch, which is the only way back on. It writes to
   the daemon's settings, not to extension storage, so one opt-out lives in one place and is
   visible in all three of the places somebody might look for it. Disabled when the daemon is
   not answering — a switch that lies about what it did is worse than one that says it
   cannot.
4. **What did it find?** Every confirmed stream on this tab, with its ladder, on the same
   one-click-per-rung rule the overlay uses so the two surfaces cannot teach different
   habits.

### The transfer list

Unfinished jobs, newest first, at most five and then a count of the rest, each one a button
that opens that job in the app (§2c). Under them, *Open Vortex*, which does the same with no
job named.

It is **deliberately meagre**: a filename, a state, a byte count and a 2 px rule. No speed,
no ETA, no segment map. Two reasons, and they point the same way.

The first is mechanical. Speed and ETA live on `SummaryFrame`, which means a `Subscribe`,
which means 2 Hz per job into a service worker whose whole cost model is that it sleeps after
thirty idle seconds. So the panel polls `List` while it is open — roughly once a second,
stopping when the popup is destroyed on blur — and renders what a `JobView` already carries.
`JobView.completed` is updated by the daemon on every progress event even though it is not
broadcast, so the bytes are current; nothing shown here is a stale number dressed up as a
live one.

The second is that the segment map is the one thing in Vortex worth being remembered for
(05 §Signature), and a 380 px reimplementation of it would be a worse copy competing with
the real one. The panel's job is to say *that* a transfer exists, name it, and be one click
from the window that draws it properly. Its progress rule is achromatic for the same reason:
a rounded percentage is not the segment map, and colouring it would be borrowing the
signature's colour for a summary of it.

The list re-renders **only its own rows** on each poll. The rest of the panel is redrawn
wholesale, which is right for a window that is open for a few seconds — but doing it here
would take the *Added to Vortex* a rung is showing out from under the pointer.

### The panel arrives

The browser opens this window the instant the button is clicked, and it cannot be filled in
that instant: the active tab has to be found and the background worker has to be woken, which
under MV3 is sometimes the slowest thing that happens all session (§Service worker lifetime).
So the frame is blank and then, tens of milliseconds later, fully furnished — which reads as
a stall followed by a jolt, and reads *worse* the longer the wait was.

Six pixels and 200 ms per section, each one a beat behind the one above, turns that into a
panel unfolding. It is settled inside a quarter of a second; it is not a performance, it is
the difference between content that appeared and content that was placed. Only ever on the
first draw — toggling the site switch redraws the whole panel, and replaying an entrance for
that would claim the window had just opened when it had not.

Everything else here follows 05 §Motion, including the three per cent a press takes off
whatever is pressed. The progress rule is the one thing that stays still on purpose: it is
repainted on a poll roughly once a second, and easing between two of those samples would draw
a speed that is an artefact of the polling rather than of the transfer.

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
