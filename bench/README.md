# bench

Two suites, both answering questions the product's front page makes claims about.

- **`hostile-server/`** — a server that misbehaves on demand. Every case must yield a correct
  file or a clean `NeedsDecision`; none may yield silent corruption. Driven by
  `crates/vortex-engine/tests/hostile.rs`.
- **`throughput/`** — the speed harness. `cargo run --release -p vortex-bench`.

> Every scheduler change runs the throughput harness. No exceptions, because scheduler
> changes that help one case routinely wreck another.
>
> — `docs/06-roadmap.md`

It exits non-zero when a fixture falls through its floor, so it is a gate and not only a
report.

---

## Running it

```sh
# The offline fixtures. About four minutes.
cargo run --release -p vortex-bench

# With the real-network fixture, and a markdown table for the README.
cargo run --release -p vortex-bench -- \
    --cdn-url https://dl.google.com/go/go1.23.4.windows-amd64.zip \
    --markdown bench.md

# One fixture, more trials, on a specific drive.
cargo run --release -p vortex-bench -- --fixture small --n 25 --dir E:\
```

**Release, always.** A debug-mode engine measured against a release-mode `curl` is not a
measurement of anything.

`--size` overrides every local fixture's size. Sweeping it is how you find out whether a gap
is fixed cost or a throughput difference, and those want very different fixes.

---

## The fixtures

| Fixture | What a passing number establishes | Held to |
|---|---|---|
| `uncapped` | Sixteen connections cost nothing where there is no headroom to win. | ≤ 250 ms over `curl` |
| `throttled` | The scenario this category was built on — a server shaping bandwidth per connection. | ≥ 3.00× |
| `small` | The parallelism gate: a 4 MiB file is not worth sixteen connections. | 1 connection, ≤ 100 ms over `curl` |
| `backpressure` | Memory stays bounded when the network outruns the writer. | < 256 MB resident growth |
| `cdn` | The same no-regression floor against a real CDN, over a real network, with real TLS. | ≥ 0.98× |
| `long-haul` | h3 beats h2 on a lossy 200 ms path, and both beat one stream. | *not runnable here* |
| `heterogeneous` | Eviction and hedging beat naive fanout when one edge is slow. | *not runnable here* |

The last two print as `not run`, with the reason, rather than being quietly omitted. A
harness that drops a fixture it cannot run reads as though it measured everything.

- **`long-haul`** needs `tc netem delay 200ms loss 1%`, which is Linux only. Shaping the
  origin instead would not reproduce it: netem constrains the congestion window, and a
  server that paces its own writes does not. For an uncontrolled but real version, point
  `--cdn-url` at a mirror on another continent.
- **`heterogeneous`** needs one hostname resolving to several addresses, which means a hosts
  entry and therefore an administrator. The engine resolves through the system resolver by
  design, and adding a test-only override to production code to dodge that is the wrong
  trade. The corruption this fixture would have found was caught by `backpressure` instead,
  and is now pinned by `one_slow_edge_among_fast_ones_still_produces_a_byte_exact_file` in
  the hostile suite.

### Choosing a `--cdn-url`

There is no default, on purpose. CI must not depend on somebody else's CDN, third-party URLs
rot, and the right origin depends on where you are. It needs to be:

- **range-capable** — a `206` to `curl -r 0-15`, or the engine correctly falls back to one
  stream and the fixture measures nothing;
- **indifferent to concurrency** — speedtest mirrors commonly `429` past two connections, at
  which point you are measuring the `throttled` fixture over the internet;
- **near enough that your own link is the bottleneck** — point it at a distant mirror and you
  have measured long-haul, which is a different claim;
- **large enough that fixed cost is under about 1%** — see below. 06 specifies a 5 GB object
  for exactly this reason.

---

## Method

- **A discarded warm-up pair**, then `n` measured pairs. The first run of anything pays for a
  cold page cache, a cold DNS answer and a file that has never been allocated.
- **Alternating order** — Vortex first on even trials, `curl` first on odd ones. A machine
  warms up, a CDN caches, a laptop throttles; running five Vortex trials and then five
  baseline trials measures the drift as much as the scheduler.
- **Paired comparison** — the ratio and the overhead are computed per trial and then
  summarised, so a machine that drifts mid-run drifts for both sides.
- **Median with p10/p90.** At five samples the tails sit just inside the extremes, which is
  the point of printing them: a 3.1× whose p10 is 1.2× is not a 3.1×, it is a bimodal result
  with a story behind it.
- **Every trial is hashed.** Both sides must produce the same bytes, and a local fixture's
  hash is checked against what the origin knows it served. A faster number from a corrupt
  file is not a number.
- **One engine per fixture**, built before the clock starts. `vortexd` is a long-lived
  process: a user's download does not pay for building a rustls config or reading the system
  proxy settings. The cost is that `reqwest` keeps two idle connections per host, so up to
  two of sixteen workers may skip a handshake `curl` always pays — microseconds on loopback,
  one handshake in sixteen on a real CDN.
- **Both sides send the same `User-Agent`.** Real CDNs treat `curl/8` and a browser string
  differently, and that difference is not what is being measured.

### Why the loopback fixtures are held to milliseconds rather than a ratio

`curl` returns when its last write reaches the page cache. Vortex returns when the bytes are
on the device — it fsyncs the data, appends a journal record, fsyncs that, and renames off
`.vxpart`, because claim #1 is that killing the process loses nothing. That costs a bounded
amount, measured here at **roughly 120 ms**, and it is not optional.

On a real transfer it disappears: 120 ms against a 40-second download is 0.3%. On loopback,
where 256 MB moves in a quarter of a second and 4 MiB moves in three milliseconds, it is most
of the wall clock — so a ratio there is a measurement of how fast the machine can fsync, not
of the scheduler. The fixed cost is the scale-free form of the same promise, and it is the
one that catches the regressions that matter: an added fsync, a slower probe, a concurrency
decision that costs seconds.

06's ratio floor is on `cdn`, over a real network, where 06 put it.

Per-trial output breaks the clock into `probe`, `transfer` and `tail`. `tail` is everything
after the last progress frame — the final fsync, the last journal record and the rename, plus
however much transfer fell inside the last 50 ms frame interval. It is a diagnostic, not a
clean measure of finalisation, and it is named so that it does not pretend to be one.

---

## What this harness found

Everything below was live in the engine, passed the full test suite, and was found by
running these fixtures once.

1. **Silent corruption under hedging.** A hedge that found its current block already complete
   advanced its cursor to the next block *without consuming the matching bytes from its HTTP
   response*, so every later chunk was written shifted. Full-length file, wrong content,
   non-deterministic. Two runs in three at 256 MiB with a starved write budget; zero in
   twenty-four after the fix. Nothing else in the suite made a straggler, so nothing else
   reached that code.
2. **A supported setting that cost 20× throughput.** The writer flushed at a flat 8 MiB or
   every 250 ms, and the daemon clamps its memory budget to a *minimum* of 8 MiB — at which
   the volume trigger can never fire, because some of the budget is always in the channel
   rather than in the pending map. The 250 ms timer became the throttle: 32 MB/s. 7.5 s for
   256 MiB, against 0.40 s after.
3. **Fanout onto an unreachable address family.** Many real machines have IPv6 that is
   configured but not routable. `curl` and every browser survive that through Happy Eyeballs;
   fanout pinned one worker per resolved address and sat in connect timeouts. Against a real
   CDN: **40.8 s where `curl` took 6.3 s** for the same 100 MB. The probe already connects
   unpinned and therefore already knows which family works — it was throwing the answer away.
4. **A concurrency controller that could only ascend.** Its own module doc says it settles at
   two connections on a saturated CDN. It started at four and had no way back down: a step
   that bought nothing was kept, and the count plateaued on top of it. On a link a single
   stream already saturates, every extra connection is measurably worse — 1 connection 2.18 s,
   2 → 2.61 s, 4 → 3.06 s, 6 → 3.87 s for the same object — and Vortex ran at five. 0.70×
   against `curl`, where the README promises a tie.
5. **A control loop reading a display estimate.** The controller compared throughput three
   seconds after a change against throughput before it, using an estimate with a three-second
   half-life — so it was comparing a settled reading against a half-settled one, and a step
   that truly doubled throughput measured as about +50%. It concluded doubling had not paid
   and walked up one connection at a time. Against a per-connection-throttled origin it
   plateaued at five where it should have reached the ceiling: 4.19×, against 6.25× after.
6. **A panic in the daemon.** Seeding leases used `clamp(cursor + block, end)`, and almost
   every real file ends in a partial block — when a split landed a boundary on the last whole
   one, the floor was above the ceiling and `clamp` panicked, taking every other download with
   it. Which boundaries occur depends on the connection count, which is why it survived until
   #4 changed the starting count.

Each is now pinned by a test that fails without its fix.
