# 04 — Media Pipeline

Turning a manifest and a few thousand segments into one file the user can open.

---

## Why `blob:` is a red herring

The most common user complaint — "the video is a `blob:` URL, I can't save it" — describes
a symptom, not the obstacle. `blob:` is what Media Source Extensions produces when
JavaScript feeds segments into a `SourceBuffer`. The *segments* and the *manifest* that
describes them are ordinary HTTP requests, fully visible to `webRequest`.

Solve the manifest and blob video solves itself. There is no `blob:` problem.

---

## The stages

```
manifest URL
   │
1  fetch + parse         HLS master / DASH MPD
2  ladder                enumerate variants, codecs, audio groups, subtitle tracks
3  select                default to the user's current playback resolution
4  resolve               expand to a concrete, ordered segment list
5  fetch                 through the SAME scheduler as any other job
6  decrypt               AES-128 only, in flight
7  mux                   ffmpeg -c copy → MP4 or MKV
8  finalize              metadata, atomic rename
```

Stages 5 and 8 are the engine (02). Stages 1–4 and 6–7 are `vortex-media`.

---

## 1–2. Parsing and the ladder

Own parser, not a wrapper. The manifest *is* the product surface here — the ladder shown in
the overlay comes directly from it, and outsourcing that means outsourcing the UX.

**HLS** (RFC 8216)

| Tag | Why it matters |
|---|---|
| `#EXT-X-STREAM-INF` | the variant ladder: bandwidth, resolution, `CODECS` |
| `#EXT-X-MEDIA` | separate audio/subtitle renditions — the reason naive downloaders produce silent video |
| `#EXT-X-MAP` | init segment. Omit it and the output is unplayable |
| `#EXT-X-BYTERANGE` | byte-range segments within one file |
| `#EXT-X-KEY` | encryption — AES-128 handled, DRM refused |
| `#EXT-X-PLAYLIST-TYPE`, `#EXT-X-ENDLIST` | VOD vs live |
| `#EXT-X-DISCONTINUITY` | timeline breaks — mux must respect them or A/V drifts |

**DASH** (ISO/IEC 23009-1)

| Element | Why it matters |
|---|---|
| `AdaptationSet` / `Representation` | the ladder |
| `SegmentTemplate` + `$Number$` / `$Time$` | most common addressing |
| `SegmentTimeline` | required for correct `$Time$` expansion; guessing produces gaps |
| `SegmentBase` + `indexRange` | single-file representations — parse the `sidx` box |
| `ContentProtection` | **DRM → refuse** |
| `@type="dynamic"` | live |

Ladder entries carry an *estimated* size: `bandwidth × duration / 8`. Label it as an
estimate in the UI. Manifest bandwidth is a peak declaration, and real files routinely land
20–30% under it. Showing "1.2 GB" and delivering 890 MB is fine; showing it as exact is a
small lie the user will notice.

## 3. Selection

Default = the variant nearest the resolution currently playing, from the MSE hook. Not the
maximum. Someone saving a lecture wants 1080p, not the 4 GB HEVC master.

Audio: highest-bitrate rendition in the selected variant's `AUDIO` group, defaulting to the
page language. Subtitles: fetch all, convert WebVTT → SRT, embed as soft tracks in MKV or as
`mov_text` in MP4.

## 4–5. Fetching

Segments are just jobs. They go through the same scheduler, with a different shape:

| | File download | Segment set |
|---|---|---|
| Unit | 1 MiB block | 1 segment (2–10 s of media) |
| Concurrency | adaptive 4→32 | adaptive 8→24 |
| Ordering | any | **in-order preferred**, so mux can start early |
| Retry | per block | per segment, and a segment may be fetched from an alternate host if the manifest lists one |

Segment counts get large — a 2-hour VOD at 4 s segments is 1,800 requests. Therefore:

- Reuse connections aggressively; h2/h3 multiplexing genuinely helps here, unlike on a
  single large file.
- A missing segment is **not** a job failure. Retry with backoff; if it stays gone, mark the
  gap, keep going, and report "3 of 1,800 segments unavailable" at the end rather than
  discarding 40 minutes of successful work.
- Live streams: poll the media playlist on its `EXT-X-TARGETDURATION`, append, and let the
  user stop whenever. Write incrementally so a 6-hour capture is never held in memory.

## 6. AES-128

Clear-key HLS (`METHOD=AES-128`) delivers its key over HTTP with the same session
credentials as the manifest. Fetch it with the captured envelope, decrypt each segment in
flight (`AES-128-CBC`, IV from `#EXT-X-KEY:IV` or the segment's media sequence number), and
write plaintext.

`SAMPLE-AES` **with a DRM key format is refused** (03 §Denylist). `SAMPLE-AES` with a plain
clear key is technically the same class as AES-128; it is supported.

## 7. Muxing

```
ffmpeg -i video.m4s -i audio.m4s -i subs.vtt \
       -c copy -c:s mov_text \
       -movflags +faststart \
       output.mp4
```

- **`-c copy` always.** Never re-encode by default. Re-encoding is slow, lossy, and not what
  was asked for. Transcoding is an explicit, separate, opt-in action.
- **Container choice:** MP4 when codecs allow (H.264/HEVC + AAC). Automatically MKV when
  they don't (VP9 + Opus, or multiple subtitle tracks), with the reason shown: *"Saved as
  MKV — MP4 can't hold Opus audio."* Silently producing a file that won't open in the user's
  player is a support ticket.
- **`+faststart`** so the file is seekable immediately.
- Progress is parsed from ffmpeg's `-progress pipe:1` and shown as a distinct **Muxing**
  phase in the UI. On a 4 GB remux this takes 20–60 s, and an unexplained stall at 100% is
  the most common "it's broken" report in every downloader ever shipped.
- Segments are kept until mux succeeds and verifies. Only then are they deleted.
- Mux failure is recoverable: the segments are on disk, the job shows **Retry mux**.

## 8. Naming

Priority order, first hit wins:

1. `Content-Disposition: filename*` (RFC 5987, UTF-8 aware)
2. Manifest title / DASH `ProgramInformation`
3. Page `<title>`, with site-name suffixes stripped
4. URL path basename

Then sanitize per 01 §Security, dedupe with ` (2)`, and cap total path length at 240 chars
on Windows — reserving room for the `.vxpart` suffix so the temp path never exceeds `MAX_PATH`
before the final file does.

---

## `yt-dlp` as the extractor fallback

Vortex's own parser handles standards-compliant HLS and DASH — the large majority of the
long tail. It will not handle sites with bespoke obfuscation, and it should not try.

```
Vortex parser succeeds  →  Vortex scheduler downloads it   (the fast path)
Vortex parser fails     →  yt-dlp --dump-json              (extract only)
                        →  Vortex scheduler downloads it   (still the fast path)
```

**yt-dlp extracts; it never downloads.** Vortex takes the resolved URLs and runs them
through its own scheduler, so adaptive concurrency, hedging, resume and the writer all still
apply. yt-dlp's own downloader would give up every one of those.

**An extraction returns one of two things, and they are different jobs.**

| What came back | What happens next |
| --- | --- |
| **A ladder of finished URLs** — one file for the video, one for the audio | A *progressive* plan: each URL is probed for its length, cut into 4 MiB blocks, and handed to the same fetcher a manifest's segments go to. Nothing re-fetches the page. |
| **A manifest** — an `.m3u8` or `.mpd` the sniffer never saw | Straight back to `plan::inspect`. Our own parsers build the real ladder, with the audio renditions, the subtitle tracks and the byte ranges that a flattened list of formats has already thrown away. |

The two are never mixed into one ladder. A large site publishes both families at once —
YouTube currently answers with thirty-odd progressive URLs *and* a dozen HLS playlists for
the same video — and a rung that is a playlist sitting next to a rung that is an MP4 with
the same number written on it means whichever the extractor listed last decides how *all*
of them get fetched. Direct URLs win when there are any: they carry exact sizes and the
complete resolution ladder.

**A progressive block that goes missing stops the run.** A lost HLS segment costs four
seconds of video and the file still plays, so the fetcher records a gap and carries on
(§5). One file cut into byte ranges has no such slack — a hole in the middle of an MP4 is
not a shorter MP4, it is a file no player will open — so everything contiguous stays on
disk and the retry picks up from exactly that block.

**The container is learned rather than declared.** An extractor hands back URLs and no
codec strings, so the usual codec test has nothing to weigh. What the server answers with
does: a WebM body is VP9 and Opus, neither of which goes into an MP4, so the file is saved
as MKV — and, as everywhere else, the reason is shown rather than left as a surprise
extension.

Sidecar, bundled and self-updating on its own channel — extractors break weekly and cannot
wait for an app release. `scripts/ytdlp.mjs` fetches the platform build at bundle time and
verifies it against the `SHA2-256SUMS` the release publishes; nothing third-party is
vendored into the repository, and a checksum mismatch fails the build rather than warning.
`YtDlp::find()` looks beside the daemon first, then `$VORTEX_YTDLP`, then `PATH`.

**The extractor's ladder is not shown as it arrives.** A site like YouTube publishes three
encodes of every resolution — H.264, VP9 and AV1 at 144p, at 240p, and so on up — which is
twenty-five rows with `1080p` written on three of them. That is not a choice, it is a quiz,
so each resolution keeps one format: the most playable codec, and the better encode where
the codec ties. The default audio track is the loudest one *in the first language listed*,
because taking the first outright is a 4K download with 48 kbps audio and taking the loudest
outright is a Portuguese dub of an English talk.

The extension reaches this path through channel 5 (03 §5), which offers the daemon a page
URL when a page has a player and nothing on the wire explained it.

⚠️ Site-specific extractors ship in the **daemon**, never in the store extension (03 §Store).

---

## The DRM boundary

Vortex does not download DRM-protected video. Not with a flag, not in a "developer mode",
not behind an advanced setting.

**Why this is a product decision, not caution:**

- Widevine, PlayReady and FairPlay decrypt inside a CDM sandbox or a hardware TEE. Getting
  keys out requires circumventing a technological protection measure.
- DMCA §1201 penalizes circumvention, and penalizes **distributing the tool** separately and
  more harshly. Vortex is a distributed tool.
- Google actively issues DMCA takedowns against Widevine circumvention repositories.
- No extension store will list it, and the store is mandatory for Chrome users.
- One DRM feature makes the entire product unshippable, unsellable, and unfundable.

**What remains in scope is not small.** Clear HLS and DASH covers news, sports, education,
conference talks, government and municipal video, corporate training, self-hosted video,
and the very long tail of sites that just use hls.js or Video.js. That is the market.

The four names in the exclusion list are four companies. The rest of the web is the product.
