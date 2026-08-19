# 02 — The Engine

The engine is everything below the UI and below capture. Given a `RequestEnvelope`, produce
a correct file on disk as fast as the network allows, and never enter a state a restart
can't recover from.

**Buy the transport. Build the scheduler.** `hyper`/`rustls`/`quinn` are the genuinely hard
60% and are already solved. What follows is the other 40%, which is where every download
manager actually differentiates.

---

## 1. Transport

### Protocol selection

```
first contact with an origin
        │
        ├── happy-eyeballs race:  h2 over TCP  vs  h3 over QUIC (250 ms head start for TCP)
        │
        ├── winner recorded in the per-origin policy cache (TTL 7 days)
        └── Alt-Svc from any response updates the cache
```

HTTP/3 is a real advantage on lossy paths — QUIC's per-stream loss recovery avoids TCP
head-of-line blocking, which is exactly the long-haul international case. It is also the
riskiest dependency in the stack: **`reqwest`'s h3 support is still gated behind
`reqwest_unstable` and is not stabilized.** So:

- h1.1 and h2 go through `reqwest`/`hyper` — the stable, boring, correct path.
- h3 is implemented directly on `quinn` + the `h3` crate, behind `VortexTransport`.
- h3 is **never load-bearing**. Any QUIC handshake failure, `NO_ERROR` close, or throughput
  regression versus the h2 baseline demotes the origin to h2 for 24 hours, silently.
- A single settings toggle disables h3 globally. If h3 ever becomes a support burden, it
  turns off in one release without touching the scheduler.

> This is the trap that sinks the "faster than Chrome" claim. Chrome speaks h3 to every
> major CDN. An engine that only speaks HTTP/1.1 — which is what wrapping **aria2** would
> give you, its h2 issue has been open since 2015 — can lose 1-vs-8 on a lossy QUIC path.
> Vortex does not have that ceiling.

### Connection discipline

- **rustls** with the aws-lc-rs provider; TLS 1.3 session resumption and 0-RTT where the
  origin allows it. On a 200 ms RTT path, resumption saves a full RTT per connection, and
  with 16 connections that is not a rounding error.
- Connection pool keyed by `(origin, alpn, resolved_ip)`.
- `Accept-Encoding: identity`, always. Content coding on a ranged request destroys byte
  offsets, and some CDNs will happily gzip a 206.
- Every request carries the captured envelope verbatim: `User-Agent`, `Referer`, `Origin`,
  `Cookie`, `Sec-Fetch-*`, and any custom auth header. Divergence from what the browser sent
  is the number one cause of a 403 on handoff.

### Multi-IP fanout

Resolve the origin to the full A/AAAA set, then distribute workers across **distinct
addresses**. On an anycast CDN this frequently lands workers on different edge machines and
raises the aggregate ceiling well above what one socket to one PoP can deliver. It is also
free resilience: one bad edge node degrades one worker instead of the job.

Re-resolve on a `ServFail`, on a worker eviction, and every 5 minutes on long jobs.

---

## 2. Probe

Probing is where correctness is won or lost. Servers lie.

```
1. GET with `Range: bytes=0-0`         ← not HEAD. Many CDNs mishandle or reject HEAD,
                                         and HEAD tells you nothing about range behavior.
2. Follow redirects, record the FINAL url — that is the one workers use.
3. Classify the response:
     206 + Content-Range: bytes 0-0/N   → ranges work, size known         → PARALLEL
     206 + Content-Range: bytes 0-0/*   → ranges work, size unknown       → SINGLE, streaming
     200 + full body                    → server ignored Range            → SINGLE
     200 + Content-Length, no ranges    → SINGLE
     416                                → retry without Range, treat as SINGLE
4. Capture validators:
     ETag        strong preferred. A W/ weak tag is UNUSABLE for resume across CDN
                 nodes — record it, but never rely on it alone for If-Range.
     Last-Modified
     Content-Length, Content-Type, Content-Disposition
     Digest / Repr-Digest / x-amz-checksum-* / x-goog-hash  → integrity, if present
5. LIAR CHECK: issue a second probe at `Range: bytes=<N/2>-<N/2+15>`.
   Verify the returned Content-Range matches the request and the 16 bytes are not
   byte-identical to offset 0. Costs one round trip. Catches the class of proxy that
   returns 206 with the wrong body — which otherwise silently corrupts the file.
```

Probe results are cached per-origin (not per-URL) for range support, concurrency plateau,
and preferred ALPN.

---

## 3. Scheduler

The differentiator. Chrome splits statically into 3. aria2 splits at midpoints. Vortex does
neither.

### Data model: blocks under leases

```
File = fixed-size BLOCKS (1 MiB default; 4 MiB above 4 GB)
       ↓
Ground truth = a roaring bitmap of completed blocks
       ↓
Workers hold contiguous LEASES over block ranges
```

Blocks make resume, stealing and out-of-order completion trivial and keep persisted state
tiny. Contiguous leases keep each socket's read pattern sequential, which is what TCP
congestion control and the disk both want. You need both.

### Lease assignment

Initial: split the file into `initial_workers` equal leases. Trivial, and immediately
corrected by stealing.

**Steal on demand.** When a worker finishes its lease, it does not exit. It asks the
scheduler for work, and the scheduler splits the lease with the **largest predicted
remaining time**, not the largest byte count:

```
predicted_remaining(lease) = bytes_left(lease) / ewma_throughput(owner)
```

The split point equalizes the two predicted finish times rather than halving the bytes:

```
split_offset = start + bytes_left * ( r_new / (r_owner + r_new) )
```

where `r` is measured throughput. A fast idle worker takes a proportionally larger share
from a slow one. Midpoint splitting — aria2's approach — hands half the remaining work to a
worker that may be four times slower, and the tail gets worse instead of better.

Minimum steal size: 4 MiB. Below that, splitting costs more in handshakes than it saves.

### Adaptive concurrency — gradient ascent, not a constant

This is the mechanism that beats a fixed connection count in both directions.

```
start at 2 connections
every 3 s:
    measure throughput on a 750 ms EWMA — not the 3 s one on screen
    gain = (now - before the last step) / before the last step
    if gain > 50%   → double        (the origin is shaping per connection)
    if gain >  5%   → add one       (still paying, but not proportionally)
    else            → give the step back, plateau there
    if throughput DROPPED after a step                      → give the step back, mark origin
    if any worker sees 429 / 503 / connection reset         → halve, mark origin
cap: min(user_max, origin_plateau_from_cache, 32)
```

Four things in that block are there because measuring it went badly first (`bench/README.md`):

- **Start at 2, not 4.** The loop decides every 3 s, so a transfer of a few seconds gets one
  decision or none and runs at whatever it started on. On a link a single stream already
  saturates, every extra connection is measurably *worse* — 1 connection 2.18 s, 2 → 2.61 s,
  4 → 3.06 s, 6 → 3.87 s for the same CDN object. Starting at four bet on parallelism before
  any evidence and lost 30% of the transfer on the case this product promises to tie.
- **Give a step back.** A step that bought nothing used to be *kept*, and the count plateaued
  on top of it, paying for those connections for the rest of the transfer.
- **Double while it is clearly paying.** Ascending one at a time is what made starting at 4
  seem necessary. Doubling reaches the ceiling in four steps, which is what makes starting
  low affordable.
- **Control off a fast estimate.** The rate on screen is smoothed over 3 s so it does not
  flicker. Reading *that* one tick after a change compares a settled number against a
  half-settled one: a step that truly doubled throughput measured as about +50%, so the
  controller kept deciding that doubling had not paid.

Consequences that matter:

- Against a **per-connection-throttled** host, throughput keeps climbing and the controller
  keeps adding. It finds 16 or 24 on its own — given a transfer long enough to get there.
- Against an **uncapped anycast CDN**, the extra connection yields nothing and is handed
  straight back, so Vortex adds no handshake overhead where there was nothing to win. This is
  how the "never slower than Chrome" guarantee is enforced rather than asserted.
- Against a host that **punishes** concurrency with 429s, it halves and remembers, so the
  next download from that origin starts near the right number.

The learned plateau is written to the per-origin policy cache. The second download from a
host is optimal from the first second — which is also what makes starting low cheap: a host
that rewards concurrency pays for the discovery once.

**Known limit.** A transfer shorter than about two intervals never gets a decision, so short
downloads run at the starting count. That is why the number matters so much, and the real fix
is a control loop that can decide inside the first second — which needs a sample measured in
bytes rather than seconds, and is not built.

### Tail hedging

The last few percent of a segmented download is where wall-clock goes to die: fifteen
workers idle while one straggler finishes.

```
when completed / total > 0.95:
    for each in-flight lease whose predicted finish > p95 of the others:
        issue a DUPLICATE request for that range on a fresh connection
        first response to land wins; the loser is cancelled
```

Costs a small amount of duplicate bandwidth on a small fraction of the file. Eliminates the
straggler tail, which on heterogeneous paths is routinely 10–30% of total wall-clock.

### Slow-worker eviction

```
if worker_throughput < 0.2 * median_worker_throughput  for > 5 s:
    cancel it, return its remaining lease to the pool,
    reconnect against a DIFFERENT resolved IP
```

A worker that landed on a bad edge node stays bad. Replacing it is nearly always right.

### When NOT to parallelize

Hard gates, checked before a single extra socket opens:

| Condition | Mode |
|---|---|
| No range support | single stream |
| Size unknown | single stream, streaming write |
| Size < 8 MiB | single stream |
| Predicted duration < 2 s | single stream |
| Origin marked `hostile_to_concurrency` | single stream |
| Weak-only validator **and** resuming | re-probe; if uncertain, restart rather than corrupt |

---

## 4. The writer

Sixteen sockets producing data at random offsets is a disk access pattern from hell. The
writer's whole job is to hide that.

```
workers ──(offset, Bytes)──► mpsc ──► WRITER THREAD ──► pwrite / WriteFile
                                            │
                                     reorder + coalesce
```

- **One dedicated OS thread per job.** Never a tokio task (see 01 — Threading).
- **Reorder window:** hold pending buffers in a small `BTreeMap<offset, Bytes>`, flush when
  ≥8 MiB of *adjacent* data has accumulated or 250 ms elapse. Sixteen random writers become
  one near-sequential writer.
- **Coalesce** adjacent buffers into a single `writev`/`WriteFile` call.
- **Backpressure, not buffering.** The channel is bounded by a global memory budget
  (default 256 MiB). When it fills, workers stop reading their sockets. TCP flow control
  then does the rest. Memory use is bounded regardless of the disk/network ratio — this is
  what keeps a 10 Gbps link from OOMing the machine on a slow drive.
- **Never mmap.** Page-fault storms, no backpressure, and pathological behavior on Windows
  with large sparse files.

### Preallocation

| Platform | Call |
|---|---|
| Windows | `SetFileInformationByHandle(FileAllocationInfo)` + `FSCTL_SET_SPARSE` |
| Linux | `fallocate(FALLOC_FL_KEEP_SIZE)` |
| macOS | `fcntl(F_PREALLOCATE)` then `ftruncate` |

**Do not use `SetFileValidData`.** It requires `SE_MANAGE_VOLUME_NAME`, which means
admin — and it exposes stale on-disk contents in the unwritten region, which is a genuine
information-disclosure bug. Marking the file sparse achieves the goal (no zero-fill pass)
with no privilege and no leak.

### Windows-specific

Defender's real-time scan-on-write is frequently the actual bottleneck on Windows, not the
network and not the disk. Two mitigations:

1. Large coalesced writes (already the design) reduce scan invocations dramatically.
2. Offer, at install time and **only with explicit consent**, a Defender exclusion for the
   `.vxpart` extension. Present it as an opt-in checkbox with a plain explanation, never as
   a silent default. It stays a user's choice.

### Completion

```
all blocks complete
  → fsync data file
  → verify size == Content-Length
  → verify checksum, if the server offered one
  → atomic rename  .vxpart → final name   (ReplaceFile / rename(2))
  → delete .meta
```

The final filename never exists until the file is complete and verified. A partially written
file is never mistakable for a finished one.

---

## 5. Resume and durability

### The `.meta` file

```
magic "VXPT" | version | flags
final_url, origin_url, method, header set, validators
total_size, block_size
── checkpoint: roaring bitmap of completed blocks ──
── journal: append-only run records since the last checkpoint ──
```

**Write ordering is the whole game:**

```
data written  →  fsync(data)  →  append journal record  →  fsync(meta)
```

Data before metadata, always. The failure mode of the reverse order is a `.meta` claiming
blocks that never reached the platter — silent corruption that surfaces months later. Full
bitmap checkpoint every 30 s or 256 MiB, whichever first; the journal is truncated on
checkpoint.

Crash recovery: replay the journal onto the checkpoint. A torn journal record at the tail is
discarded — costs at most a few re-fetched blocks.

### Revalidation on resume

```
If-Range: "<strong-etag>"        (or If-Unmodified-Since when no strong tag exists)

206 → continue from the bitmap
200 → THE FILE CHANGED. Never merge. Emit NeedsDecision:
      [ Start over ]  [ Keep the old partial as a separate file ]  [ Cancel ]
```

Silently merging bytes from two different file versions is the single worst thing a download
manager can do, because the user finds out at extraction time, weeks later.

### Survives

| Event | Mechanism |
|---|---|
| Process kill / power loss | journal + checkpoint replay |
| Wi-Fi → LTE, VPN toggle, IP change | OS network-change subscription (`NotifyIpInterfaceChange` on Windows, `NWPathMonitor` on macOS, netlink on Linux) → tear down and rebuild immediately, rather than waiting out a 60–120 s TCP timeout. This alone is the difference between "instant" and "hung" in the most common real-world interruption. |
| Sleep / wake | same path, plus a monotonic-clock check to invalidate stale throughput EWMAs |
| Disk full | pause with `NeedsDecision`, nothing truncated |
| Signed URL expiry | `UrlExpired` → extension re-mints (below) |
| Daemon restart | scan download roots for `.vxpart.meta`, rehydrate |

---

## 6. Failure taxonomy

Every error is classified before anything is retried. This table is the reason claim #1
holds.

| Class | Triggers | Response |
|---|---|---|
| **Transient** | connection reset, timeout, 5xx, 429, QUIC handshake failure | exponential backoff with full jitter, per-worker budget; the job never fails because a worker did |
| **Renewable** | 403 / 401 on a URL that previously worked, expired-signature bodies, `X-Amz-Expires` elapsed | emit `UrlExpired` → the extension re-acquires a fresh URL in page context → `RenewedUrl` → **continue from the existing bitmap** |
| **Integrity** | validator changed, checksum mismatch, size mismatch | stop immediately, never merge, `NeedsDecision` |
| **Fatal** | 404, 410, 400 with no retry semantics | terminal — the only state where a job legitimately fails |
| **Local** | disk full, permission denied, path too long | pause, `NeedsDecision`, fully resumable |

**Renewable is the one that matters most in practice.** Expired signed URLs are IDM's
single biggest real-world failure: the download dies at 80% and the user starts over. Vortex
has something IDM doesn't — a live browser session on the other end of the pipe. The
extension revisits the originating page or replays the originating request in page context,
gets a fresh signed URL for the same object, and the engine resumes against the existing
bitmap. Nothing is re-downloaded.

### Degradation ladder

A job walks down this ladder rather than failing:

```
N connections
   → fewer connections        (concurrency controller backs off)
   → 1 connection             (hostile origin)
   → 1 connection, h2 forced  (h3 demoted)
   → Stalled + resumable      (retry with backoff, indefinitely, in the background)
   → Fatal                    (only for 404/410 — a genuinely gone resource)
```

### Circuit breakers

Per-origin, not per-job. Ten consecutive transient failures against an origin opens the
breaker for 60 s; all jobs on that origin pause together instead of sixteen workers each
hammering a struggling server. Half-open probe on expiry.

---

## 7. Integrity

- Always: bytes written == `Content-Length`.
- When offered: verify `Digest` / `Repr-Digest` (RFC 9530), `x-amz-checksum-*`,
  `x-goog-hash`, or a Metalink hash. Computed incrementally during the write, so
  verification costs nothing at the end.
- Surface the result. A verified download gets a checkmark with the algorithm named on
  hover. An unverifiable one gets nothing — never a fake green tick.

---

## 8. Benchmarking the claims

Any change to the scheduler must be validated against `bench/`:

| Fixture | Purpose |
|---|---|
| Uncapped anycast CDN | prove ≈1.0× — the no-regression floor |
| `nginx` with `limit_rate` per connection | prove 3–8× — the real IDM scenario |
| `tc netem` 200 ms RTT + 1% loss | prove the long-haul case, and that h3 beats h2 there |
| Heterogeneous multi-IP origin | prove hedging and eviction earn their complexity |
| **Hostile server suite** | the correctness half — see below |

The hostile suite is a purpose-built server that: advertises `Accept-Ranges` and ignores
`Range`; returns 206 with the wrong `Content-Range`; changes its `ETag` mid-transfer; sends
`GOAWAY` at random; truncates responses short of `Content-Length`; gzips a 206; and returns
a valid-looking body of zeros. Every one of these must produce a correct file or a clean
`NeedsDecision`. None may produce silent corruption.

**Speed is a feature. Correctness under a lying server is the product.**
