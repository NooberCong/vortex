# 06 — Build Plan, Benchmarks, Risks

## Sequencing principle

Build the thing that can invalidate the product first. In order of "would kill it":

1. **Handoff works** — can `vortexd` actually re-issue a browser request and get bytes?
2. **The scheduler beats a single stream** — is the speed claim real?
3. **It survives hostile servers** — does it corrupt files?
4. **HLS/DASH muxes cleanly** — is the video story real?
5. Everything else.

Phase 0 exists to answer #1 in a week, before any UI is designed and before any crate
structure is committed to.

---

## Phase 0 — Kill the risk (1 week)

A throwaway spike. Delete it afterward.

- A 200-line MV3 extension: `webRequest` observer, `downloads.onCreated` → cancel → native
  message with the full envelope.
- A 300-line Rust host: receive envelope, `reqwest` GET, write to disk.
- Test against **20 real sites** covering the hostile cases: S3 presigned URLs, Cloudflare,
  Google Drive, a corporate SharePoint, an academic mirror, a forum attachment, a
  `Content-Disposition` with UTF-8, a POST-initiated download.

**Exit criterion:** ≥ 80% of the 20 hand off successfully. Below that, the header/cookie
replay model needs rethinking before anything else is built.

## Phase 1 — Engine core (4 weeks)

`crates/vortex-engine`, headless, driven by a CLI harness. No UI, no daemon, no extension.

| | |
|---|---|
| Week 1 | Probe (02 §2) including the liar check. Block bitmap. Single-stream download. `.meta` write ordering. |
| Week 2 | Lease scheduler, throughput-weighted stealing, worker eviction. |
| Week 3 | The writer thread — reorder, coalesce, backpressure, preallocation, sparse files. |
| Week 4 | Failure taxonomy, degradation ladder, circuit breakers, resume across kill/reboot. |

**Exit:** the hostile-server suite passes 100%. Not "mostly" — a silent corruption bug found
in phase 5 costs ten times what it costs here.

## Phase 2 — Adaptive concurrency + transport (2 weeks)

- Gradient-ascent concurrency controller, per-origin policy cache.
- Tail hedging.
- Multi-IP fanout.
- h2 via `reqwest`; h3 via `quinn` + `h3` behind `VortexTransport`, with automatic demotion.
- **The benchmark harness lands here**, and the numbers in the README get replaced with
  measured ones.

**Exit:** ≥ 3× on the throttled fixture, and **≥ 0.98×** on the uncapped fixture. The second
number is the important one — it is the "never slower" guarantee, and it is the one a
scheduler change is most likely to break.

## Phase 3 — Daemon, IPC, extension (3 weeks)

- `vortexd`: IPC server, SQLite, job lifecycle, rehydration from `.vxpart.meta`.
- `vortex-proto` with `ts-rs` export.
- `vortex-host`: stdio ⇄ named pipe.
- Real extension in WXT: MV3 + MV2 targets, request ledger, takeover, DRM denylist,
  daemon-unreachable passivity.
- **URL renewal** end to end — this is the differentiating resilience feature and it needs
  both halves to exist.

**Exit:** a download survives daemon restart, browser restart, and Wi-Fi→LTE switching,
with no user action.

## Phase 4 — Media (3 weeks)

- HLS and DASH parsers, ladder extraction, `EXT-X-MEDIA` audio groups, `SegmentTimeline`.
- Segment fetching through the existing scheduler.
- AES-128 in-flight decryption.
- ffmpeg sidecar muxing with progress, container auto-selection.
- yt-dlp fallback — **extract only**.
- MSE content-script hook.
- DRM refusal in both layers.

**Exit:** 30 real video sites produce a playable file with correct audio. Audio is where
naive implementations fail, so it is the acceptance criterion.

## Phase 5 — Interface (4 weeks)

- Tauri 2 shell, frameless window, Mica, tray, taskbar progress.
- Svelte 5 frontend against the generated TS types.
- The segment map canvas — collapsed, expanded, the shatter.
- Main list, New Download sheet, expanded row, settings, empty state.
- The page overlay with the ladder.
- Full keyboard nav, both themes, reduced motion.

**Exit:** the quality floor in 05 — 60 fps at 40 jobs, <0.5% idle CPU, <400 ms cold start.

## Phase 6 — Ship (3 weeks)

- Windows installer (WiX), EV code signing, per-user install, no admin.
- Native messaging registration for Chrome, Edge, Firefox, Brave, Vivaldi, Opera.
- Auto-update (minisign, Tauri updater); yt-dlp on its own faster channel.
- Store submissions — Firefox/AMO first, then Chrome with the conservative build.
- Crash reporting, opt-in telemetry (throughput only, no URLs, ever).

**Built, the OS-integration half.** `crates/vortex-setup` plus `vortexd --register` /
`--unregister`, and a per-user NSIS bundle that calls them. Four decisions worth recording:

* **Registration is a reconciliation, not an install step.** `vortexd` re-runs it on every
  start. The case is ordinary — a browser installed a month after Vortex — and the symptom
  is the worst kind: capture goes passive and says nothing, exactly like a crashed daemon.
* **It is a Rust crate rather than an installer custom action.** An installer action is
  reachable only by running an installer, so it cannot be tested, and the same work has to
  happen on macOS and Linux where there is no NSIS at all. What is left in `hooks.nsh` is
  four `nsExec` lines.
* **NSIS, not WiX.** Tauri's MSI is per-machine only, and "no admin rights at install time"
  is a constraint from 01 §Process model rather than a packaging preference. An MSI for the
  enterprise path can be added later; it cannot be the default.
* **The login entry is written by the installer, not left to the daemon.** An install is
  preceded by an uninstall — Tauri's NSIS runs the old uninstaller before copying anything,
  on an update and on a reinstall alike — and that hook's `--unregister` takes the entry
  with it. Restoring it only at the daemon's next start is a rule with a hole exactly the
  shape of the bug: restart the machine after an update and the entry that would have
  started the daemon is the entry the daemon would have written. So `--register` sets it
  from `settings.json`, and from the default — on — when there is no file yet. The daemon
  still reconciles, and now compares against `autostart_enabled()` rather than its own
  memory, so an entry deleted by anything else is repaired the next time a client connects.
* **`vortexd.exe` is a windowless binary, and the two commands that print borrow the
  caller's console.** Explorer starts a console-subsystem program with a console, so the
  login entry — the one thing that starts the daemon without a parent to hide it, since
  `vortex-ipc` spawns with `CREATE_NO_WINDOW` — was the one path that put a black window on
  the desktop for the whole session. The subsystem is the fix; `AttachConsole` in `main` is
  the part that keeps `vortexd --register` readable to someone who typed it, and leaves the
  installer's pipe alone so `ExecToLog` still has a log to write. Debug builds keep their
  console.
* **The login entry starts the app in tray mode, not the daemon.** The daemon is what has to
  be running and for a while the entry named it — correct, and indistinguishable from
  failure: a headless process leaves nothing in the tray, nothing in the taskbar and no
  window, so the first thing a user does after signing in is check whether Vortex started,
  find no evidence, and conclude it did not. `vortex-app --tray` opens no window either, but
  it leaves an icon and a throughput tooltip, and it starts the daemon on the way up the
  same way a double-click does. One entry, both processes, and quitting from the tray still
  leaves the daemon downloading. The cost is a webview resident for the session; the
  alternative was a second tray implementation inside the daemon, kept in sync with this
  one, with no macOS or Linux equivalent.
* **The extension declares its own key, so its id is arithmetic rather than an accident.**
  Chromium derives an id from the public key and, absent one, from the folder an unpacked
  build was loaded from — different on every machine, so nothing could be registered ahead
  of time. `apps/extension/identity.ts` fixes it; the id is compiled into
  `vortex_setup::CHROMIUM_IDS` and a test fails if the two drift. The store build ships no
  key: that id belongs to the listing, and joins the list as a second entry once the item
  exists.

**Not built yet:** EV code signing, the updater (minisign, pinned endpoint), store
submissions, crash reporting and opt-in telemetry.

Three sidecars now ship alongside the two cargo builds, each fetched and checksum-verified
at bundle time: **yt-dlp** (`scripts/ytdlp.mjs`), the **qjs** JavaScript engine it borrows
for YouTube's player challenge (`scripts/qjs.mjs`), and **ffmpeg** (`scripts/ffmpeg.mjs`),
which is ~115 MB and the reason the installer is what it is. macOS gets the first two; see
04 §Muxing for why its ffmpeg is left to `PATH`.

What is still missing is yt-dlp's *own release channel* — today it is pinned to whatever
`latest` was when the installer was built, and updating it means shipping an app release,
which is exactly what a weekly-breaking extractor cannot wait for. The same now goes for
qjs, though it moves far more slowly.

**Total: ~20 weeks solo.** Roughly 5 months for a credible product, which is the honest
number for this category. Phases 1–2 can compress if the engine is the only focus; phases
5–6 do not compress, because polish and installers are not parallelizable against yourself.

---

## Benchmark harness (`bench/`)

Every scheduler change runs this. No exceptions, because scheduler changes that help one
case routinely wreck another.

### Speed fixtures

| Fixture | Setup | Asserts |
|---|---|---|
| **Uncapped CDN** | 5 GB object on Cloudflare/Fastly, gigabit local link | ≥ 0.98× vs `curl`. **The no-regression floor.** |
| **Per-connection throttle** | `nginx` + `limit_rate 5m` | ≥ 3× |
| **Long-haul lossy** | `tc netem delay 200ms loss 1%` | ≥ 1.5×, and h3 ≥ h2 |
| **Heterogeneous multi-IP** | 4 backends, one deliberately slow | eviction + hedging beat naive by ≥ 15% |
| **Small file** | 4 MiB | ≥ 0.98× — proves the parallelism gate works |
| **Slow disk** | throttled HDD, 10 Gbps source | memory stays under the 256 MiB budget |

Protocol: alternating order, n ≥ 5, report median and p10/p90. Never test only the throttled
fixture — that is how a project ends up believing its own marketing.

**Built, in `bench/throughput/`.** Three notes from making the numbers real, all of them in
`bench/README.md` at length:

* The loopback fixtures are gated on **milliseconds over the baseline**, not on a ratio.
  `curl` returns when its last write reaches the page cache; Vortex returns when the bytes are
  on the device. That is a bounded cost, about 120 ms, and on a link with no bottleneck it is
  most of the wall clock — so a ratio there measures how fast the machine can fsync. The
  ratio floor lives on the uncapped-CDN fixture, where this table puts it.
* **Long-haul** and **heterogeneous multi-IP** are not runnable without `tc netem` and a
  multi-address hostname. The harness prints them as `not run` with the reason rather than
  omitting them.
* The uncapped-CDN object has to be **large enough that the fixed cost is under about 1%**,
  which is why this table says 5 GB. On a smaller object the 0.98× floor is unreachable no
  matter how good the scheduler is.

### Hostile-server suite

A purpose-built server that misbehaves on demand. Every case must yield a **correct file** or
a **clean `NeedsDecision`**. None may yield silent corruption.

```
advertises Accept-Ranges, ignores Range
returns 206 with a WRONG Content-Range
returns 206 with the body of a DIFFERENT offset
changes ETag mid-transfer
sends HTTP/2 GOAWAY at random intervals
truncates the body short of Content-Length
gzips a 206 response
returns 200 to an If-Range that should give 206
returns a plausible body of all zeros
drops the connection at exactly 99% every time
issues 429 after the 5th concurrent connection
signs URLs that expire 30 seconds in
```

That last one is the URL-renewal acceptance test.

---

## Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| **Chrome Web Store pulls the extension** | High | Firefox is the flagship. `vortexd` works standalone with clipboard monitoring and paste-URL. Nothing in the architecture depends on the Chrome listing surviving. Assume it won't. |
| **Handoff fails on modern CDNs** | High | Phase 0 measures this before anything is built. Probe-before-erase means a failed takeover is invisible to the user. URL renewal covers the expiry case. |
| **h3 via `quinn`/`h3` is immature** | Medium | Never load-bearing. Auto-demotes to h2 on any anomaly, and a single setting kills it globally. `reqwest`'s h3 stays unstable, which is precisely why it isn't the foundation. |
| **Speed claim doesn't survive contact with real CDNs** | Medium | Then say so. The README already frames ≈1.0× as an expected outcome. Resilience, capture and muxing carry the product; speed is a sometimes-feature. A downloader that overpromises speed gets benchmarked and called a liar. |
| **ffmpeg licensing** | Medium | Separate sidecar process, LGPL build, no static linking. Legal review **before** first public build. |
| **Defender destroys Windows throughput** | Medium | Large coalesced writes by design; opt-in `.vxpart` exclusion with an honest explanation. Never silent. |
| **Scope creep into DRM** | High | It is a stated product boundary in three documents. One DRM feature makes the product unshippable, unsellable, and unfundable. There is no version of this where it is worth it. |
| **Torrent support pulls focus** | Low | Deliberately deferred past 1.0. It is a different product with different failure modes, and `rqbit` will still be there later. |

---

## Explicitly out of scope for 1.0

Not "no" forever — "not until the core is undeniable."

- BitTorrent and magnet links
- Browser-independent proxy interception (a system proxy is a support nightmare)
- Mobile
- Cloud sync of the queue
- Any form of DRM handling
- Transcoding beyond remux
- A scriptable plugin system

---

## Definition of done for 1.0

Six statements. Each is testable, and none is about a feature list.

1. A 5 GB download survives process kill, reboot, Wi-Fi→LTE, and a signed-URL expiry, and
   finishes without the user doing anything.
2. The hostile-server suite passes 100%, with zero silent corruptions.
3. Measured ≥ 3× on the throttled fixture and ≥ 0.98× on the uncapped one.
4. 30 real video sites produce a playable file with correct audio.
5. 60 fps with 40 jobs, under 0.5% idle CPU, under 400 ms cold start.
6. A user who has never seen the app downloads a video from a page in under 10 seconds
   without reading anything.
