# 01 — Architecture

## Process model

Three processes. One owns state; the other two are clients.

| Process | Lifetime | Owns | Can crash without loss? |
|---|---|---|---|
| **`vortexd`** | login → logout | Every job, every byte, every setting | No — this is the one that must not |
| **`vortex-app`** (Tauri) | user opens/closes it | Nothing. Pure view + command sender | Yes |
| **`vortex-host`** | spawned by the browser per profile | Nothing. Stateless relay | Yes |

`vortexd` is a **user-session background process**, not a Windows Service. Deliberate:

- No admin rights at install time.
- No Session 0 isolation — it shares the user's network context, proxy settings, and
  certificate store, which matters behind a corporate MITM proxy.
- Per-user config and per-user download directories work without impersonation games.

Autostart: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` on Windows, a LaunchAgent on
macOS, an XDG autostart entry on Linux. All three are user-scoped and user-removable.

**Idle behavior:** with no active jobs and no connected client, `vortexd` drops to a single
IOCP/epoll wait and roughly 8 MB RSS. It does not poll. It does not phone home.

---

## Why not the obvious alternatives

**Engine inside the Tauri app.** Closing the window kills the transfer, and a frontend panic
kills the transfer. Both violate claim #1. Rejected.

**Local HTTP server on a fixed port (XDM's `127.0.0.1:9614`).** Three problems: the port
collides, Windows Firewall prompts on first run, and any page in any browser can probe and
drive it. Mitigating the third needs a bearer token *and* strict Origin checks *and*
DNS-rebinding defense — three chances to get security wrong for zero benefit. Rejected.

**Native messaging only, no daemon.** The browser owns the host process lifetime, so closing
the browser kills the download. Also caps host to browser messages at 1 MB. Rejected as the
*primary* channel; kept as the browser transport.

---

## IPC

### Transport

| Platform | Endpoint |
|---|---|
| Windows | `\\.\pipe\vortex.SID` — DACL grants the owning SID only |
| macOS / Linux | `$XDG_RUNTIME_DIR/vortex.sock`, mode `0600` |

Framing: 4-byte little-endian length prefix, MessagePack body. MessagePack rather than JSON
because a live segment-map frame at 20 Hz across 12 jobs is a lot of numbers, and binary
bitmaps do not want base64.

### Protocol

Defined once in `crates/vortex-proto`, exported to TypeScript with `ts-rs` so the UI and the
extension cannot drift from the daemon.

```rust
// client → daemon
enum Command {
    Submit(JobSpec),
    Pause(JobId), Resume(JobId), Cancel(JobId), Remove(JobId, DeleteFile),
    Reprioritize(JobId, Priority),
    Probe(RequestEnvelope),        // "what is this URL?"    → New Download sheet
    ProbeMedia(RequestEnvelope),   // "what streams here?"   → page overlay
    Subscribe(SubscriptionScope),  // Summary | Detail(JobId) | All
    GetSettings, SetSettings(Settings),
    RenewedUrl(JobId, RequestEnvelope),  // extension answering UrlExpired
}

// daemon → client
enum Event {
    JobAdded(JobView),
    JobProgress(JobId, ProgressFrame),  // ≤20 Hz, Detail subscribers only
    JobSummary(JobId, SummaryFrame),    //   2 Hz, Summary subscribers
    JobStateChanged(JobId, JobState),
    JobFinished(JobId, Outcome),
    UrlExpired(JobId, RenewalHint),     // → extension re-mints the URL
    MediaFound(TabId, Vec<MediaCandidate>),
    NeedsDecision(JobId, Decision),     // server file changed, disk full, name conflict
    SettingsChanged(Settings),
}
```

**Subscription scope matters.** The list subscribes to `Summary` at 2 Hz. Only an expanded
row subscribes to `Detail`, which carries the per-worker block bitmap at 20 Hz. An idle
window costs the daemon nothing. This is the difference between a 0.3% and a 9% CPU floor
with 40 jobs listed.

### `ProgressFrame` — the segment map on the wire

```rust
struct ProgressFrame {
    total: u64,
    completed: u64,
    bps: u64,                 // EWMA, 3s half-life
    eta_secs: Option<u32>,
    connections: u8,
    // run-length encoded block bitmap: (start_block, len, owner)
    // owner: 0 = complete, 1..=N = in flight by worker N
    runs: Vec<(u32, u32, u8)>,
}
```

RLE, not a raw bitmap. A 4 GB file at 1 MiB blocks is 4096 blocks, but in practice there are
under 40 runs because workers hold contiguous leases. A frame is roughly 300 bytes.

---

## State ownership

`vortexd` is the only writer of anything durable.

```
%LOCALAPPDATA%\Vortex\                    (Windows)
~/.local/share/vortex/                    (Linux)
~/Library/Application Support/Vortex/     (macOS)
├── vortex.db      SQLite WAL — job records, history, per-origin policy cache
├── settings.json  human-editable, hot-reloaded
└── logs/          rotating, 7 days, structured JSON
```

Partial data lives **next to the destination file**, not in the app directory:

```
<download-dir>/Ubuntu 24.04.iso.vxpart        sparse data file
<download-dir>/Ubuntu 24.04.iso.vxpart.meta   validators + block bitmap + journal
```

Two reasons. The user can move a half-finished download to another drive and it still
resumes. And a corrupted `vortex.db` costs history, not bytes — the `.meta` file is
self-describing and sufficient to rebuild a job from nothing.

---

## Security boundaries

| Boundary | Control |
|---|---|
| Page JS → daemon | **Impossible.** No TCP listener exists. A page cannot open a named pipe. |
| Extension → daemon | Through `vortex-host`, whose native-messaging manifest pins `allowed_origins` to exact extension IDs. The platform forbids wildcards. |
| Extension identity | Manifest `key` field pins the extension ID across dev reloads and store publish, so host registration never breaks. |
| Daemon → disk | Writes confined to configured download roots plus system temp. Server-supplied `Content-Disposition` filenames are sanitized: strip separators, reject reserved Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`), cap each component at 255 UTF-8 bytes, strip trailing dots and spaces. |
| Cookies in transit | Forwarded envelopes live in memory only, zeroized on completion, never written to `vortex.db` or logs. The tracing layer redacts `Cookie`, `Authorization`, and `Set-Cookie` by header name. |
| Sidecars | `ffmpeg` and `yt-dlp` invoked with an explicit argv array, never a shell string. No format string is ever derived from an untrusted manifest. |
| Updates | minisign-signed, public key compiled in, Tauri updater with a pinned endpoint. |

---

## Threading

```
vortexd
├── main            IPC accept loop, command dispatch
├── tokio runtime   N = physical cores, work-stealing
│   ├── scheduler task   (1 per job)   leases, stealing, hedging
│   ├── worker tasks     (N per job)   socket read → Bytes → writer channel
│   └── probe tasks      short-lived
├── writer thread   (1 per job) — dedicated OS thread, blocking positional writes
└── meta thread     (1, shared) — journal appends, checkpoints, fsync
```

The writer is a **dedicated OS thread, not a tokio task.** A blocking write on a slow disk
must never occupy a runtime worker; at 16 connections against a 5400 rpm drive it would
starve the sockets and collapse throughput. This is the single most common performance bug
in Rust download managers, and it is invisible on an NVMe dev machine.

---

## Failure isolation

| What dies | What happens |
|---|---|
| One worker connection | Its unfinished lease returns to the pool; a replacement opens against a different resolved IP |
| All workers on a job | Degrade to single-stream, then to `Stalled` with full resumable state |
| `ffmpeg` sidecar | Segments are already on disk. Mux retries, then surfaces as a manual action |
| Tauri app | Nothing. Transfers continue. Reopening resubscribes and repaints. |
| `vortex-host` | Browser respawns on the next event. Queued envelopes replay from `chrome.storage.session`. |
| `vortexd` | On restart, every `.vxpart.meta` under the download roots is scanned and jobs rehydrate. Blocks that were in flight are re-fetched, never trusted. |
| Disk full | Job pauses with `NeedsDecision`. Nothing truncated, nothing lost. |
