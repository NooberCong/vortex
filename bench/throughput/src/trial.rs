//! One timed transfer, run twice: once through Vortex, once through the baseline.
//!
//! Both sides are timed the same way — wall clock around the whole thing, including the
//! probe, the connect, and the final rename — because that is the number a user
//! experiences. Timing only the byte-moving part would quietly excuse Vortex's two extra
//! probe round trips, which are a real cost it has to earn back.
//!
//! The engine is built once per fixture and reused, and that is deliberate. `vortexd` is a
//! long-lived process: a user's download does not pay for building a rustls config, loading
//! the platform root store, or reading the system proxy settings, because the daemon did
//! that at startup. Timing it per trial would measure this harness's architecture rather
//! than the product's — on a 4 MiB loopback fixture it is most of the wall clock.
//!
//! The cost of that choice is connection reuse: `reqwest` keeps two idle connections per
//! host, so up to two of sixteen workers may skip a handshake that `curl`, a fresh process
//! every time, always pays. On loopback that is microseconds. On a real CDN it is one TLS
//! handshake out of sixteen, and it is the reason the `cdn` fixture is the honest one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use vortex_engine::job::{self, Engine, TransferSpec};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_proto::RequestEnvelope;

use crate::rss;

const FILENAME: &str = "bench.bin";

/// Both sides send it, so an origin cannot shape one differently from the other. Real CDNs
/// do treat `curl/8` and a browser string differently, and that difference is not what this
/// harness is measuring.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36                           (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// Where a Vortex job's wall clock went. On any real link this is a rounding error; on
/// loopback, where 4 MiB of transfer takes single-digit milliseconds, it is most of the
/// job — and knowing which part grew is the difference between "the scheduler regressed"
/// and "somebody added an fsync".
pub struct Phases {
    /// Up to the moment the engine committed to a filename, which is everything the probe
    /// does: the initial request, the liar check, and resolution.
    pub probe: Duration,
    /// After the last progress frame — the final fsync, the journal's last record and the
    /// rename off `.vxpart`, plus however much transfer fell inside the last 50 ms frame
    /// interval. Not a clean measure of finalisation, and reported under a name that does
    /// not claim to be one. It is a diagnostic: when it grows, the disk did.
    pub tail: Duration,
}

pub struct Run {
    pub elapsed: Duration,
    pub bytes: u64,
    pub sha256: String,
    /// What the engine reported about the path it took: protocol, connections, addresses.
    /// `None` for the baseline, which is not asked to explain itself.
    pub detail: Option<String>,
    /// Resident growth during the transfer. `None` for the baseline — its memory belongs
    /// to another process — and on platforms this harness cannot measure.
    pub growth: Option<u64>,
    pub phases: Option<Phases>,
    /// The most connections the engine ever had open at once. This is how the small-file
    /// fixture checks the parallelism gate: not by timing it, which on loopback measures
    /// the disk, but by watching what the engine actually did.
    pub connections: Option<u8>,
}

/// One engine per fixture, built before the clock starts. See the module note.
pub fn engine() -> Arc<Engine> {
    Arc::new(Engine::new(Transport::new(TransportConfig {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        ..Default::default()
    })))
}

pub async fn vortex(
    engine: &Arc<Engine>,
    url: &str,
    dir: &Path,
    connections: u8,
    write_budget: u64,
) -> Result<Run> {
    // The engine narrates itself, so the harness does not have to guess where the time
    // went: `Destination` lands the moment the probe is done, and the last `Progress`
    // lands when the last byte is in the writer.
    let observed: Arc<Mutex<Observed>> = Arc::default();
    let (tx, mut rx) = mpsc::channel(256);
    let watched = observed.clone();
    let drain = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let mut observed = watched.lock().expect("no panic holds this lock");
            match event {
                job::EngineEvent::Destination { .. } => observed.probed = Some(Instant::now()),
                job::EngineEvent::Progress(frame) => {
                    observed.last_frame = Some(Instant::now());
                    observed.connections = observed.connections.max(frame.connections);
                }
                _ => {}
            }
        }
    });
    // Held, not dropped: the control handle is the job's lifeline, and dropping it is how
    // a caller cancels.
    let (_control, control_rx) = job::control();

    let mut envelope = RequestEnvelope::new(url);
    envelope.set_header("User-Agent", USER_AGENT);
    let spec = TransferSpec {
        envelope,
        dest_dir: dir.to_path_buf(),
        filename: Some(FILENAME.into()),
        max_connections: connections,
        write_budget,
    };

    let watch = rss::Watch::start();
    let started = Instant::now();
    let done = job::transfer(engine.clone(), spec, tx, control_rx)
        .await
        .map_err(|e| anyhow!("vortex did not finish: {e:?}"))?;
    let elapsed = started.elapsed();
    let growth = watch.stop();
    drain.abort();

    let observed = observed.lock().expect("no panic holds this lock");
    let phases = observed.probed.map(|probed| Phases {
        probe: probed.duration_since(started),
        tail: observed
            .last_frame
            .map_or(Duration::ZERO, |last| elapsed - last.duration_since(started)),
    });
    let connections = Some(observed.connections);

    let detail = format!(
        "{}, {} address(es), {} retries ({} recovered)",
        done.protocol.map_or("unknown protocol", |p| p.label()),
        done.addresses,
        done.retries.0,
        done.retries.1
    );
    finish(&done.path, elapsed, Some(detail), growth, phases, connections)
}

pub async fn baseline(url: &str, dir: &Path) -> Result<Run> {
    let path = dir.join(FILENAME);
    let started = Instant::now();
    let output = tokio::process::Command::new("curl")
        // `-sS` keeps the progress meter off stdout without hiding a real error, and `-L`
        // matches the engine, which follows up to ten redirects.
        .args(["-sS", "-L", "-A", USER_AGENT, "--output"])
        .arg(&path)
        .arg(url)
        .output()
        .await
        .context("running curl")?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        bail!(
            "curl exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    finish(&path, elapsed, None, None, None, None)
}

/// Is there a baseline to compare against at all? Asked once, so a missing `curl` is a
/// sentence at startup rather than five identical failures.
pub async fn baseline_available() -> bool {
    tokio::process::Command::new("curl")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Hashes the result and takes the file away again, so the next trial starts on an empty
/// directory and the disk does not slowly fill with 256 MiB fixtures.
fn finish(
    path: &Path,
    elapsed: Duration,
    detail: Option<String>,
    growth: Option<u64>,
    phases: Option<Phases>,
    connections: Option<u8>,
) -> Result<Run> {
    let (bytes, sha256) = digest(path)?;
    let _ = std::fs::remove_file(path);
    Ok(Run {
        elapsed,
        bytes,
        sha256,
        detail,
        growth,
        phases,
        connections,
    })
}

fn digest(path: &Path) -> Result<(u64, String)> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        total += read as u64;
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}

/// What the event stream said, collected while the transfer runs.
#[derive(Default)]
struct Observed {
    probed: Option<Instant>,
    last_frame: Option<Instant>,
    connections: u8,
}

/// A scratch directory that removes itself, so an interrupted run leaves nothing behind
/// on the drive under test.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(root: &Path, name: &str) -> Result<Self> {
        let dir = root.join(format!("vortex-bench-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
