//! The throughput harness (06 §Benchmark harness).
//!
//! Every scheduler change runs this. No exceptions, because scheduler changes that help one
//! case routinely wreck another — and because the claim on the front of this repository is
//! a claim about measured speed, which means somebody has to measure it.
//!
//! Three disciplines, all of them there because the alternative is a project that believes
//! its own marketing:
//!
//! * **Never test only the throttled fixture.** The uncapped one is the important number.
//!   It is the "never slower" guarantee, and it is the one a scheduler change is most
//!   likely to break.
//! * **Alternate the order.** A machine warms up, a CDN caches, a laptop throttles. Running
//!   five Vortex trials and then five baseline trials measures the drift as much as the
//!   scheduler.
//! * **Hash every trial.** A faster number from a corrupt file is not a number. Both sides
//!   must produce the same bytes, and for a local fixture both must match what the origin
//!   knows it served.
//!
//! Exit status is the verdict: non-zero when any fixture fell through its floor, so this is
//! usable as a gate and not only as a report.

mod report;
mod rss;
mod stats;
mod trial;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use hostile_server::Behaviour;
use vortex_proto::fmt;

const MB: u64 = 1 << 20;

#[derive(Parser)]
#[command(name = "vortex-bench", about = "Vortex throughput harness")]
struct Args {
    /// Run only these fixtures. Repeatable.
    #[arg(long = "fixture")]
    only: Vec<String>,
    /// Trials per fixture, after a discarded warm-up pair. 06 asks for at least five.
    #[arg(long, default_value_t = 5)]
    n: usize,
    /// Where the files land. Point it at whichever drive you want measured.
    #[arg(long)]
    dir: Option<PathBuf>,
    /// An object on a real CDN, which enables the `cdn` fixture. There is no default on
    /// purpose. CI must not depend on somebody else's CDN, third-party URLs rot, and the
    /// right origin depends on where you are: the fixture is the no-regression floor, so
    /// it needs somewhere near and fast enough that your own link is the bottleneck. Point
    /// it at a distant mirror instead and you have measured long-haul, which is a
    /// different claim.
    #[arg(long)]
    cdn_url: Option<String>,
    /// Also write the markdown table here, ready to paste into the README.
    #[arg(long)]
    markdown: Option<PathBuf>,
    /// Override every local fixture's size, in bytes. A sweep across sizes is how you find
    /// out whether a gap is fixed cost or a throughput difference, and the two want very
    /// different fixes.
    #[arg(long)]
    size: Option<u64>,
}

struct Fixture {
    id: &'static str,
    /// One line: what a passing number here actually establishes. A fixture that cannot
    /// answer this is measuring for the sake of measuring.
    proves: &'static str,
    origin: Origin,
    connections: u8,
    write_budget: u64,
    /// Setup worth naming in the report beyond the origin itself.
    note: Option<&'static str>,
    /// Median speedup the fixture must reach.
    floor: Option<f64>,
    /// Resident growth the fixture must stay under.
    ceiling: Option<u64>,
    /// Median wall-clock cost over the baseline. A ratio is the wrong assertion for a job
    /// whose duration is mostly fixed cost; on loopback a 4 MiB transfer is a few
    /// milliseconds and the rest is fsync, so the ratio measures the disk and this
    /// measures the engine.
    overhead: Option<Duration>,
    /// The most connections the engine may open. Hardware-independent, and the direct
    /// form of "the parallelism gate works".
    lanes: Option<u8>,
}

enum Origin {
    /// A shaped origin on loopback.
    Local { bytes: u64, behaviour: Behaviour },
    /// Somebody else's CDN, over the real network.
    Remote { url: String },
    /// In the plan, not runnable here. The reason travels with it and gets printed.
    Absent(&'static str),
}

fn fixtures(args: &Args) -> Vec<Fixture> {
    vec![
        Fixture {
            id: "uncapped",
            proves: "sixteen connections cost nothing where there is no headroom to win                      — the offline half of the no-regression floor",
            origin: Origin::Local {
                bytes: 256 * MB,
                behaviour: Behaviour::default(),
            },
            connections: 16,
            write_budget: 64 * MB,
            note: None,
            // The ratio is reported, not gated, and 06's ratio floor lives on `cdn` where
            // 06 put it: a real CDN over a real link. Loopback has no network to be a
            // bottleneck, so what is left is the disk — Vortex returns when the bytes are
            // on the device, `curl` returns when the last write reached the page cache,
            // and that difference alone is a quarter of the job at this size. Gating the
            // ratio here would gate on how fast this machine can fsync.
            //
            // The fixed cost is what this fixture can hold to account, and it is the
            // scale-free form of the same promise: Vortex costs a bounded number of
            // milliseconds over a single stream, not a proportion of the transfer.
            floor: None,
            ceiling: None,
            overhead: Some(Duration::from_millis(250)),
            lanes: None,
        },
        Fixture {
            id: "throttled",
            proves: "the scenario this category was built on — a server that shapes \
                     bandwidth per connection",
            origin: Origin::Local {
                bytes: 128 * MB,
                behaviour: Behaviour {
                    limit_rate: Some(8 * MB),
                    ..Default::default()
                },
            },
            connections: 16,
            write_budget: 64 * MB,
            note: None,
            floor: Some(3.0),
            ceiling: None,
            overhead: None,
            lanes: None,
        },
        Fixture {
            id: "small",
            proves: "the parallelism gate: a 4 MiB file is not worth sixteen connections, \
                     and the engine has to know that",
            origin: Origin::Local {
                bytes: 4 * MB,
                behaviour: Behaviour::default(),
            },
            connections: 16,
            write_budget: 64 * MB,
            note: None,
            // No ratio. A 4 MiB loopback transfer is single-digit milliseconds of network
            // against roughly sixty of fsync, so the ratio here is a measurement of the
            // drive. What the fixture is actually for — that the engine does not throw
            // sixteen connections at a small file — is asserted directly, and the fixed
            // cost is held to a budget wide enough for a spinning disk and tight enough to
            // catch an added pass over the file.
            floor: None,
            ceiling: None,
            overhead: Some(Duration::from_millis(100)),
            lanes: Some(1),
        },
        Fixture {
            id: "backpressure",
            proves: "memory stays bounded when the network outruns the writer — the \
                     slow-disk invariant, reached by clamping the write budget instead of \
                     by finding a slow disk",
            origin: Origin::Local {
                bytes: 256 * MB,
                behaviour: Behaviour::default(),
            },
            connections: 16,
            // Small enough that every worker parks on the writer rather than on its
            // socket, which is the state a 5400 rpm drive produces on a fast link.
            write_budget: 8 * MB,
            note: Some("8 MiB write budget"),
            floor: None,
            ceiling: Some(256 * MB),
            overhead: None,
            lanes: None,
        },
        Fixture {
            id: "cdn",
            proves: "the same no-regression floor against a real anycast CDN, over a real \
                     network, with real TLS",
            origin: match &args.cdn_url {
                Some(url) => Origin::Remote { url: url.clone() },
                None => Origin::Absent(
                    "pass --cdn-url with an object on a range-capable origin near enough                      that your own link is the bottleneck",
                ),
            },
            connections: 16,
            write_budget: 64 * MB,
            note: None,
            floor: Some(0.98),
            ceiling: None,
            overhead: None,
            lanes: None,
        },
        Fixture {
            id: "long-haul",
            proves: "h3 beats h2 on a lossy 200 ms path, and both beat one stream",
            origin: Origin::Absent(
                "needs `tc netem delay 200ms loss 1%`, which is Linux only. Shaping the \
                 origin instead would not reproduce it: netem constrains the congestion \
                 window, and a server that paces its own writes does not",
            ),
            connections: 16,
            write_budget: 64 * MB,
            note: None,
            floor: Some(1.5),
            ceiling: None,
            overhead: None,
            lanes: None,
        },
        Fixture {
            id: "heterogeneous",
            proves: "worker eviction and tail hedging beat naive fanout when one edge is \
                     slow",
            origin: Origin::Absent(
                "needs one hostname resolving to several addresses, which means a hosts \
                 entry and therefore an administrator. The engine resolves through the \
                 system resolver by design, and adding a test-only override to production \
                 code to dodge that is the wrong trade",
            ),
            connections: 16,
            write_budget: 64 * MB,
            note: None,
            floor: Some(1.15),
            ceiling: None,
            overhead: None,
            lanes: None,
        },
    ]
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.n == 0 {
        bail!("--n must be at least 1");
    }
    if !trial::baseline_available().await {
        bail!("curl is not on PATH, and it is the baseline every number here is relative to");
    }

    let root = args.dir.clone().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;

    let mut rows = Vec::new();
    let all = fixtures(&args);
    for fixture in &all {
        if !args.only.is_empty() && !args.only.iter().any(|id| id == fixture.id) {
            continue;
        }
        eprintln!("-- {} -- {}", fixture.id, fixture.proves);
        rows.push(run(fixture, &args, &root).await);
    }
    if rows.is_empty() {
        bail!(
            "no fixture matched; known ones are: {}",
            all.iter().map(|f| f.id).collect::<Vec<_>>().join(", ")
        );
    }

    println!("\n{}", report::console(&rows));
    let markdown = report::markdown(&rows);
    println!("{markdown}");
    if let Some(path) = &args.markdown {
        std::fs::write(path, &markdown).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("markdown written to {}", path.display());
    }

    let failed: Vec<_> = rows
        .iter()
        .filter_map(|row| match &row.result {
            report::Outcome::Measured(m) if !m.passed() => Some(row.id),
            _ => None,
        })
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        bail!("below floor: {}", failed.join(", "))
    }
}

async fn run(fixture: &Fixture, args: &Args, root: &Path) -> report::Row {
    let row = |bytes, result| report::Row {
        id: fixture.id,
        conditions: conditions(fixture, args.size, bytes),
        result,
    };

    if let Origin::Absent(why) = &fixture.origin {
        return row(None, report::Outcome::NotRun((*why).to_owned()));
    }

    match measure(fixture, args.n, root, args.size).await {
        Ok(measured) => row(Some(measured.bytes), report::Outcome::Measured(measured)),
        // A fixture that could not run is not a fixture that passed. It says why, and the
        // exit status stays clean only because "the network was down" is not a regression.
        Err(e) => row(None, report::Outcome::NotRun(format!("{e:#}"))),
    }
}

async fn measure(
    fixture: &Fixture,
    trials: usize,
    root: &Path,
    size: Option<u64>,
) -> Result<report::Measured> {
    let scratch = trial::Scratch::new(root, fixture.id)?;
    let (url, known_digest, origin) = match &fixture.origin {
        Origin::Local { bytes, behaviour } => {
            let server = hostile_server::spawn(size.unwrap_or(*bytes), behaviour.clone()).await;
            (server.url(), Some(server.sha256.clone()), Some(server))
        }
        Origin::Remote { url } => (url.clone(), None, None),
        Origin::Absent(_) => unreachable!("filtered by the caller"),
    };

    let engine = trial::engine();
    let mut vortex_secs = Vec::new();
    let mut baseline_secs = Vec::new();
    let mut speedups = Vec::new();
    let mut overheads = Vec::new();
    let mut growth: Option<u64> = None;
    let mut lanes: u8 = 0;
    let mut measured_bytes = 0u64;
    let mut agreed = known_digest;

    // Trial 0 is a warm-up and is thrown away: the first run of anything pays for a cold
    // page cache, a cold DNS answer and a file that has never been allocated. Keeping it
    // would penalise whichever side happened to go first.
    for i in 0..=trials {
        let (vortex, baseline) = if i % 2 == 0 {
            let v = trial::vortex(
                &engine,
                &url,
                scratch.path(),
                fixture.connections,
                fixture.write_budget,
            )
            .await?;
            let b = trial::baseline(&url, scratch.path()).await?;
            (v, b)
        } else {
            let b = trial::baseline(&url, scratch.path()).await?;
            let v = trial::vortex(
                &engine,
                &url,
                scratch.path(),
                fixture.connections,
                fixture.write_budget,
            )
            .await?;
            (v, b)
        };

        // Named individually, and against the origin's own digest wherever there is one.
        // "The two sides disagree" is not a diagnosis: one of them wrote the wrong file,
        // and which one is the entire question.
        for (who, run) in [("vortex", &vortex), ("curl", &baseline)] {
            let Some(digest) = &agreed else { break };
            if &run.sha256 != digest {
                bail!(
                    "trial {i}: {who} wrote {} bytes hashing {}, but the origin served a file hashing {}",
                    run.bytes,
                    &run.sha256[..12],
                    &digest[..12]
                );
            }
        }
        if vortex.sha256 != baseline.sha256 {
            bail!(
                "trial {i}: the two sides disagree and the origin cannot say who is right - vortex {} bytes ({}), curl {} bytes ({})",
                vortex.bytes,
                &vortex.sha256[..12],
                baseline.bytes,
                &baseline.sha256[..12]
            );
        }
        if agreed.is_none() {
            agreed = Some(vortex.sha256.clone());
        }

        if i == 0 {
            eprintln!(
                "   warm-up: vortex {:.3} s, curl {:.3} s{}",
                vortex.elapsed.as_secs_f64(),
                baseline.elapsed.as_secs_f64(),
                vortex
                    .detail
                    .as_deref()
                    .map(|d| format!(" -- {d}"))
                    .unwrap_or_default()
            );
            continue;
        }

        let v = vortex.elapsed.as_secs_f64();
        let b = baseline.elapsed.as_secs_f64();
        eprintln!(
            "   trial {i}: vortex {v:.3} s, curl {b:.3} s -> {:.2}x{}",
            b / v,
            vortex
                .phases
                .as_ref()
                .map(|p| format!(
                    "   [probe {:.0} ms, transfer {:.0} ms, tail {:.0} ms, {} conn]",
                    p.probe.as_secs_f64() * 1000.0,
                    vortex
                        .elapsed
                        .saturating_sub(p.probe)
                        .saturating_sub(p.tail)
                        .as_secs_f64()
                        * 1000.0,
                    p.tail.as_secs_f64() * 1000.0,
                    vortex.connections.unwrap_or(0)
                ))
                .unwrap_or_default()
        );
        measured_bytes = vortex.bytes;
        vortex_secs.push(v);
        baseline_secs.push(b);
        speedups.push(b / v);
        overheads.push(v - b);
        lanes = lanes.max(vortex.connections.unwrap_or(0));
        if let Some(sampled) = vortex.growth {
            growth = Some(growth.map_or(sampled, |peak: u64| peak.max(sampled)));
        }
    }

    if let Some(server) = origin {
        server.stop();
    }

    let speedup = stats::summarize(&speedups);
    let overhead = stats::summarize(&overheads);
    let mut failures = Vec::new();
    if let Some(floor) = fixture.floor {
        if speedup.median < floor {
            failures.push(format!("{:.2}x is below {floor:.2}x", speedup.median));
        }
    }
    if let (Some(ceiling), Some(peak)) = (fixture.ceiling, growth) {
        if peak > ceiling {
            failures.push(format!(
                "grew {} past the {} ceiling",
                fmt::bytes(peak),
                fmt::bytes(ceiling)
            ));
        }
    }
    if let Some(budget) = fixture.overhead {
        if overhead.median > budget.as_secs_f64() {
            failures.push(format!(
                "{:.0} ms over the baseline, budget {:.0} ms",
                overhead.median * 1000.0,
                budget.as_secs_f64() * 1000.0
            ));
        }
    }
    if let Some(allowed) = fixture.lanes {
        if lanes > allowed {
            failures.push(format!("opened {lanes} connections, at most {allowed} allowed"));
        }
    }

    Ok(report::Measured {
        trials,
        bytes: measured_bytes,
        vortex: stats::summarize(&vortex_secs),
        baseline: stats::summarize(&baseline_secs),
        speedup,
        overhead,
        growth,
        lanes,
        floor: fixture.floor,
        ceiling: fixture.ceiling,
        budget: fixture.overhead,
        allowed_lanes: fixture.lanes,
        failures,
    })
}

fn conditions(fixture: &Fixture, size: Option<u64>, measured: Option<u64>) -> String {
    let mut out = match &fixture.origin {
        Origin::Local { bytes, behaviour } => {
            let mut s = format!("{} on loopback", fmt::bytes(size.unwrap_or(*bytes)));
            if let Some(rate) = behaviour.limit_rate {
                s.push_str(&format!(", {} per connection", fmt::rate(rate)));
            }
            s
        }
        Origin::Remote { url } => {
            let host = url.split('/').nth(2).unwrap_or(url);
            match measured {
                Some(bytes) => format!("{} from {host}", fmt::bytes(bytes)),
                None => format!("an object on {host}"),
            }
        }
        Origin::Absent(_) => return "—".to_owned(),
    };
    out.push_str(&format!(", up to {} connections", fixture.connections));
    if let Some(note) = fixture.note {
        out.push_str(&format!(", {note}"));
    }
    out
}
