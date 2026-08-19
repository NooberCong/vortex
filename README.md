<div align="center">

<img src="apps/desktop/src-tauri/icons/128x128@2x.png" width="112" height="112" alt="Vortex" />

# Vortex

**A parallel download engine with browser capture.**<br/>
Measurably faster than your browser. Effectively impossible to break.

[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-206ae1?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Tauri 2](https://img.shields.io/badge/tauri-2-206ae1?style=flat-square&logo=tauri&logoColor=white)](https://tauri.app)
[![Windows · macOS · Linux](https://img.shields.io/badge/windows%20%C2%B7%20macOS%20%C2%B7%20linux-4a4a4a?style=flat-square)](#download)
[![Per-user install](https://img.shields.io/badge/install-per--user%2C%20no%20admin-4a4a4a?style=flat-square)](#download)
[![No telemetry](https://img.shields.io/badge/telemetry-none-4a4a4a?style=flat-square)](#privacy)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-4a4a4a?style=flat-square)](#license)

[Download](#download) · [Benchmarks](#the-performance-stance-measured) · [How it works](#system-shape) · [Documentation](#documentation) · [Status](#status)

</div>

---

```
w1 ▓▓▓▓▓▓▓▓▓▓▓▓▒▒▒▒·································  18.2 MB/s ▁▂▄▅▅▄
w2 ················▓▓▓▓▓▓▓▓▒▒▒▒····················   9.1 MB/s ▂▃▃▂▁▁
w3 ····························▓▓▓▓▓▓▓▓▓▓▓▒▒▒▒·····  21.7 MB/s ▃▅▆▇▇▆
w4 ·······································▓▓▒▒▒▒▒▒   4.4 MB/s ▁▁▂▁▁▁  ← being stolen from
```

Every other download manager draws a progress bar. Vortex draws the **actual byte-range
occupancy of the file**, live, one lane per connection — so you can watch the scheduler take
work away from a slow worker and hand it to a fast one.

---

## The three claims, in priority order

### 1. It never loses a download

Kill the process, pull the Wi-Fi, close the laptop, swap to LTE, let the signed URL expire,
run out of disk. Every one of these resumes exactly where it stopped, with no user action.
A Vortex job has no non-resumable failure state that isn't a real `404`.

### 2. It is faster wherever speed is actually available — and never slower

Not "8× faster" — that's marketing built on one scenario. Vortex finds the per-origin
throughput ceiling automatically and settles there, whether that means 24 connections or 1.
Against an uncapped anycast CDN it will tie the browser, and it will say so.

### 3. It captures what the browser hides

Every downloadable file, plus HLS and DASH video that the browser only ever renders into a
`blob:` URL. One click, one MP4.

---

## Download

Per-user install. No administrator rights, no `Program Files`, no background service nobody
asked for.

<div align="center">

| Platform | Package | Notes |
|---|---|---|
| **Windows 10/11** | [`Vortex_x64-setup.exe`](../../releases/latest) | NSIS, per-user, registers with every installed browser |
| **macOS** | [`Vortex.dmg`](../../releases/latest) | [Build from source](#building-from-source) today |
| **Linux** | [`.deb` / `.AppImage`](../../releases/latest) | [Build from source](#building-from-source) today |

</div>

The installer is not yet code-signed, so Windows SmartScreen will warn on first run
(**More info → Run anyway**). EV signing is tracked in [the roadmap](docs/06-roadmap.md).

Chrome, Edge, Firefox, Brave, Vivaldi and Opera are all registered by the installer — and
`vortexd` re-runs registration on every start, so a browser installed *after* Vortex still
works.

---

## What Vortex will not do

Vortex does not touch DRM. No Widevine, no PlayReady, no FairPlay. Netflix, Disney+, Prime
Video, Max, Hulu and their peers are **blocked at the extension layer** — the capture overlay
never appears on those origins, and the engine refuses any manifest carrying a DRM
`ContentProtection` descriptor or a `SAMPLE-AES` key format.

This is not timidity. Circumventing a technological protection measure is a DMCA §1201
violation with separate, harsher penalties for *distributing the tool*. Google actively
DMCAs Widevine circumvention repositories off GitHub. Building it would make every other
part of this product legally radioactive, and it would make the app unshippable through any
extension store.

Clear HLS/DASH — news, sports, lectures, conference talks, self-hosted video, the enormous
long tail of sites that just use hls.js — is the actual market, and it is fully in scope.

---

## The performance stance, measured

Vortex ships the harness that produced these numbers ([`bench/README.md`](bench/README.md)),
and it runs against *both* a throttled host and an uncapped CDN, alternating order, n≥5,
hashing every trial. Testing only the throttled host is how a project ends up believing its
own marketing.

Against `curl`, median of 7 alternating pairs. Windows 11, NVMe, ~370 Mbit link:

| Fixture | Condition | vs `curl` |
|---|---|---|
| `throttled` | 134 MB, server shaping 8.4 MB/s per connection | **3.15×** <sub>p10 2.97, p90 3.24</sub> |
| `throttled` | the same, 403 MB — long enough for the controller to reach its plateau | **6.25×** |
| `cdn` | 82 MB from a real CDN over a real link | **1.04×** <sub>p10 0.52, p90 1.62</sub> |
| `uncapped` | 268 MB on loopback — no network to be a bottleneck at all | **0.70×**, +119 ms |
| `small` | 4 MiB, one connection — the parallelism gate holding | **0.61×**, +26 ms |

Two things that table is saying plainly.

**The speedup depends on how long the transfer runs.** The concurrency controller decides
every three seconds, so a transfer of a few seconds gets one decision or none, and runs at
close to whatever it started on. The same shaped origin gives 3.15× on a 134 MB object and
6.25× on a 403 MB one. Making short transfers reach the plateau needs a faster control loop,
and that is not built.

**On loopback Vortex loses, by a bounded constant, and it is the right trade.** `curl` returns
when its last write reaches the page cache; Vortex returns when the bytes are on the device,
because claim #1 is that killing the process loses nothing. That costs about 120 ms, once, per
download. Against a 40-second transfer it is 0.3% and invisible. Against 256 MB of loopback it
is most of the wall clock — which is why the loopback fixtures are held to a millisecond
budget and the ratio floor lives on the real-network one.

The long-haul and heterogeneous-multi-IP fixtures are **not measured**: they need `tc netem`
and a multi-address hostname respectively. `bench/README.md` says why, and the harness prints
them as `not run` rather than omitting them.

---

## System shape

Three processes, one engine, no TCP ports.

```
┌───────────────────────┐        ┌──────────────────────┐
│  Browser              │        │  Vortex (Tauri 2)    │
│  ┌─────────────────┐  │        │  UI client only —    │
│  │ Extension       │  │        │  closing it does not │
│  │  MV3 / MV2      │  │        │  stop a transfer     │
│  └────────┬────────┘  │        └───────────┬──────────┘
└───────────┼───────────┘                    │
            │ stdio (native messaging)       │ named pipe / UDS
   ┌────────▼────────┐                       │
   │ vortex-host     │───────────────────────┤
   │ stdio ⇄ pipe    │                       │
   └─────────────────┘                       │
                              ┌──────────────▼──────────────┐
                              │  vortexd  (user session)    │
                              │                             │
                              │  scheduler · transport      │
                              │  writer · resume · media    │
                              │                             │
                              │  ─ owns all state ─         │
                              └──────────────┬──────────────┘
                                             │ sidecar
                                     ┌───────▼────────┐
                                     │ ffmpeg  yt-dlp │
                                     └────────────────┘
```

**Why a background daemon and not an engine inside the Tauri app:** because claim #1 says
the UI is allowed to crash. `vortexd` owns every transfer and every byte of state. The
window is a view.

**Why named pipes and not `127.0.0.1:9614`:** XDM's local-HTTP approach means a port
conflict, a firewall prompt, and an endpoint any web page can probe. A Windows named pipe
scoped to the user SID (and a Unix domain socket in `$XDG_RUNTIME_DIR` elsewhere) has none
of those properties and is not reachable from page JavaScript at all.

---

## Privacy

- **No TCP listener exists.** A page cannot reach the daemon; there is nothing to probe.
- **Cookies forwarded from the browser live in memory only** — zeroized on completion, never
  written to `vortex.db` or to logs. The tracing layer redacts `Cookie`, `Authorization` and
  `Set-Cookie` by header name.
- **No telemetry ships.** The only telemetry on the roadmap is opt-in and throughput-only:
  no URLs, ever.
- **Uninstall leaves the queue alone.** `%LOCALAPPDATA%\Vortex` survives, so a reinstall
  resumes what was running.

Full boundary table in [`docs/01-architecture.md`](docs/01-architecture.md).

---

## Stack

| Layer | Choice | Why this one |
|---|---|---|
| Engine | Rust — `tokio`, `hyper` 1.x, `rustls` | Buy the transport, build the scheduler |
| HTTP/1.1 + h2 | `reqwest` (stable path) | Battle-tested, correct, boring |
| HTTP/3 | `quinn` + `h3` **directly** | `reqwest`'s h3 is still behind `reqwest_unstable` — not a foundation |
| Torrent *(optional, post-1.0)* | `rqbit` as a library | Pure Rust, no C++ build chain |
| Media parsing | own HLS/DASH parser in `vortex-media` | The manifest is the product; don't outsource it |
| Extraction fallback | `yt-dlp` sidecar | 1,800 site extractors you will never out-maintain |
| Mux | `ffmpeg` sidecar, `-c copy` | Never re-encode by default |
| Desktop | Tauri 2 | Native window, small binary, sidecars, Rust IPC |
| Frontend | TypeScript + Svelte 5 | Fine-grained reactivity; 60 fps segment maps without a VDOM diff |
| Extension | TypeScript + WXT | One source, MV3 (Chrome/Edge) and MV2 (Firefox) outputs |

> [!IMPORTANT]
> **Sidecar licensing.** ffmpeg is LGPL/GPL depending on build flags, `rqbit` is Apache-2.0,
> yt-dlp is Unlicense. Ship ffmpeg as a **separate sidecar process** under an LGPL build, not
> statically linked, if Vortex is ever distributed closed-source. Get this reviewed before the
> first public build, not after.

---

## Repository layout

```
vortex/
├── crates/
│   ├── vortex-engine/      transfer core — no UI, no IPC, unit-testable
│   ├── vortex-media/       HLS/DASH manifest parsing, ladder selection, mux plans
│   ├── vortex-proto/       IPC types, single source of truth (ts-rs → TypeScript)
│   ├── vortex-host/        native messaging host: stdio ⇄ named pipe
│   ├── vortex-setup/       per-user OS integration: browser registration, login entry
│   └── vortexd/            the daemon: engine + state + IPC server
├── apps/
│   ├── desktop/            Tauri 2 shell + Svelte 5 frontend (the window)
│   └── extension/          WXT — MV3 and MV2 targets
├── packages/
│   ├── proto/              the IPC types as TypeScript (ts-rs output + a barrel)
│   └── tokens/             design tokens shared by desktop UI and page overlay
├── bench/
│   ├── hostile-server/     a server that misbehaves on demand
│   └── throughput/         the speed harness — every scheduler change runs it
└── scripts/
    ├── logo.mjs            the mark, generated into the app icon and the boot splash
    ├── sidecars.mjs        stages vortexd and vortex-host for the bundler
    └── ytdlp.mjs           fetches the yt-dlp sidecar, checksum-verified
```

---

## Building from source

Requires Rust 1.85+, Node 20+, and the Tauri 2 prerequisites for the platform.

```sh
cargo test --workspace && npm test        # 250 Rust tests, 237 TypeScript
npm run bundle                            # per-user installer, no admin rights
npm run logo                              # redraw the mark after editing scripts/logo.mjs
```

`npm run bundle` stages `vortexd` and `vortex-host` beside the window, fetches the
checksum-verified `yt-dlp` sidecar, and produces an installer that registers Vortex with
every browser on the machine — and unregisters on the way out, because a registry value
pointing at a deleted executable is worse than no registration at all. Both halves are
`vortexd --register` / `--unregister`: ordinary tested Rust rather than installer script.

The mark is generated, not drawn — eight snowflake arms pulled into a curl, which is the
file and the eight workers pulling at it. `npm run logo` writes it into `icons/vortex.svg`
and into the boot splash in `index.html`, and a test fails if either copy drifts. The rasters
come from `npm run tauri icon src-tauri/icons/vortex.svg`.

---

## Documentation

| | |
|---|---|
| [`docs/01-architecture.md`](docs/01-architecture.md) | Process model, IPC protocol, state ownership, security boundaries |
| [`docs/02-engine.md`](docs/02-engine.md) | Probe, scheduler, adaptive concurrency, writer, resume, failure taxonomy |
| [`docs/03-extension.md`](docs/03-extension.md) | Capture surface, MV3 realities, handoff, overlay, DRM denylist |
| [`docs/04-media.md`](docs/04-media.md) | HLS/DASH pipeline, AES-128, muxing, the DRM boundary |
| [`docs/05-ui.md`](docs/05-ui.md) | Design system, screens, the segment map, motion, copy |
| [`docs/06-roadmap.md`](docs/06-roadmap.md) | Phased build, benchmark harness, risk register |

---

## Status

Pre-1.0, and honest about it. The engine, daemon, IPC, extension, media pipeline, interface
and the per-user installer are built; [`docs/06-roadmap.md`](docs/06-roadmap.md) records what
each phase actually landed.

**Not built yet:** EV code signing, the auto-updater (minisign, pinned endpoint), store
submissions, crash reporting, opt-in telemetry, and a separate release channel for `yt-dlp`.
Until the updater ships, an update means running the new installer — an in-place upgrade that
keeps the queue and the settings.

1.0 is done when six things are true, none of them a feature:

1. A 5 GB download survives process kill, reboot, Wi-Fi→LTE and a signed-URL expiry, and
   finishes without the user doing anything.
2. The hostile-server suite passes 100%, with zero silent corruptions.
3. Measured ≥ 3× on the throttled fixture and ≥ 0.98× on the uncapped one.
4. 30 real video sites produce a playable file with correct audio.
5. 60 fps with 40 jobs, under 0.5% idle CPU, under 400 ms cold start.
6. A user who has never seen the app downloads a video from a page in under 10 seconds
   without reading anything.

---

## License

Dual-licensed under MIT or Apache-2.0, at your option.

<div align="center">
<sub>No ports. No admin. No telemetry. No DRM.</sub>
</div>
