//! The transfer driver: probe → schedule → fetch → journal → verify → rename.
//!
//! Everything here exists to make one sentence true — *a Vortex job has no non-resumable
//! failure state that isn't a real 404*. The supervisor never fails a job because a worker
//! failed; it walks down the degradation ladder (02 §6):
//!
//! ```text
//! N connections → fewer → 1 → 1 with h3 demoted → Stalled + resumable → Fatal
//! ```

use crate::blocks::{block_size_for, BlockMap};
use crate::concurrency::{Action, Controller, START_CONNECTIONS};
use crate::error::{Class, EngineError, Result};
use crate::integrity::{self, Algo, Expected};
use crate::meta::{MetaFile, MetaHeader};
use crate::policy::{BreakerState, PolicyCache};
use crate::probe::{self, Probe};
use crate::ratelimit::RateLimiter;
use crate::scheduler::{Lane, Lease, Scheduler};
use crate::transport::{self, ByteRange, ClientKey, Transport};
use crate::writer::Writer;
use crate::{fsalloc, naming};

use futures::StreamExt;
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, Notify};
use vortex_proto::{
    Decision, JobState, ProgressFrame, Protocol, RenewalHint, RequestEnvelope, TransferMode,
    Verification,
};

/// How long a worker keeps retrying a transient failure before it gives its lease back.
const WORKER_ATTEMPTS: u32 = 6;
/// How long the job waits for the extension to re-mint a URL before it asks the user.
const RENEWAL_TIMEOUT: Duration = Duration::from_secs(120);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
const DURABILITY_INTERVAL: Duration = Duration::from_millis(500);
const SUPERVISE_INTERVAL: Duration = Duration::from_secs(1);
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);
const CHECKPOINT_BYTES: u64 = 256 << 20;

/// Shared services a job borrows. One set per daemon.
pub struct Engine {
    pub transport: Arc<Transport>,
    pub policy: Arc<PolicyCache>,
    pub limiter: Arc<RateLimiter>,
}

impl Engine {
    pub fn new(transport: Transport) -> Self {
        Self {
            transport: Arc::new(transport),
            policy: Arc::new(PolicyCache::new()),
            limiter: Arc::new(RateLimiter::unlimited()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransferSpec {
    pub envelope: RequestEnvelope,
    pub dest_dir: PathBuf,
    /// Already chosen by the user, or `None` to derive it from the response (04 §8).
    pub filename: Option<String>,
    /// A ceiling, not a target.
    pub max_connections: u8,
    pub write_budget: u64,
}

/// What the engine tells its owner while a job runs. The daemon turns these into IPC
/// events; the CLI prints them.
#[derive(Debug)]
pub enum EngineEvent {
    Probed(Box<Probe>),
    Phase(JobState),
    Progress(ProgressFrame),
    /// The names this job settled on. Emitted before a single byte is written, because
    /// the owner has to know which `.vxpart` belongs to which job — resume reclaims an
    /// existing name, and a fresh job may have to take the next free one.
    Destination { dest: PathBuf, part: PathBuf },
    /// The URL died. The extension re-mints it in page context and answers with
    /// [`Control::renew`].
    UrlExpired(RenewalHint),
    /// Segment counts and mux progress. Only a media job emits these, but they travel the
    /// same channel so a media job is a job as far as the daemon is concerned.
    Media(vortex_proto::MediaSummary),
}

#[derive(Debug)]
pub struct Completed {
    pub path: PathBuf,
    pub bytes: u64,
    pub verified: Option<Verification>,
    pub elapsed: Duration,
    pub retries: (u32, u32),
    pub protocol: Option<Protocol>,
    pub addresses: u8,
}

/// Pause, cancel, and URL renewal. Pause and cancel are the same stop: everything durable
/// is already on disk, so resuming is just running the job again.
#[derive(Clone)]
pub struct Control {
    stop: Arc<Notify>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    renew: mpsc::Sender<RequestEnvelope>,
}

impl Control {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.stop.notify_waiters();
    }

    pub async fn renew(&self, envelope: RequestEnvelope) {
        let _ = self.renew.send(envelope).await;
    }
}

struct Stop {
    notify: Arc<Notify>,
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl Stop {
    fn is_stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    async fn wait(&self) {
        while !self.is_stopped() {
            self.notify.notified().await;
        }
    }
}

pub fn control() -> (Control, ControlRx) {
    let notify = Arc::new(Notify::new());
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = mpsc::channel(4);
    (
        Control {
            stop: notify.clone(),
            stopped: flag.clone(),
            renew: tx,
        },
        ControlRx {
            stop: Stop { notify, flag },
            renew: rx,
        },
    )
}

pub struct ControlRx {
    stop: Stop,
    renew: mpsc::Receiver<RequestEnvelope>,
}

impl ControlRx {
    /// Has the owner asked this run to stop? Pause, cancel and shutdown are the same stop:
    /// everything durable is already on disk.
    pub fn is_stopped(&self) -> bool {
        self.stop.is_stopped()
    }

    /// Resolves when the owner asks this run to stop. Cancellation-safe, so it belongs in
    /// a `select!` next to whatever work is in flight.
    pub async fn stopped(&self) {
        self.stop.wait().await
    }

    /// The next envelope the extension re-minted for this job, or `None` once the run has
    /// been asked to stop. Both halves live behind one method because a caller holding a
    /// `&mut` for the renewal cannot also hold a `&` for the stop.
    pub async fn renewal(&mut self) -> Option<RequestEnvelope> {
        tokio::select! {
            envelope = self.renew.recv() => envelope,
            _ = self.stop.wait() => None,
        }
    }
}

/// Why a run ended without producing a file.
#[derive(Debug)]
pub enum Interrupted {
    /// The user pressed pause, or the daemon is shutting down. Fully resumable.
    Stopped,
    /// A question only the user can answer. The job is intact.
    NeedsDecision(Decision),
    /// Terminal — a genuinely gone resource.
    Failed(EngineError),
}

pub type Outcome = std::result::Result<Completed, Interrupted>;

// ─────────────────────────────────────────────────────────────────────────────

struct Job {
    engine: Arc<Engine>,
    /// Replaced wholesale when the extension hands back a renewed URL.
    envelope: Mutex<RequestEnvelope>,
    origin: String,
    ranges: bool,
    if_range: Option<String>,
    part: PathBuf,
    scheduler: Arc<Scheduler>,
    writer: Arc<Writer>,
    meta: Arc<Mutex<MetaFile>>,
    addresses: Vec<SocketAddr>,
    /// Next address index to hand out — eviction always reconnects somewhere else.
    next_address: AtomicU64,
    force_h11: std::sync::atomic::AtomicBool,
    protocol: Mutex<Option<Protocol>>,
    retries: (AtomicU32, AtomicU32),
    /// Set when a worker saw 429/503/reset since the last controller tick.
    pushback: std::sync::atomic::AtomicBool,
    connections: AtomicU8,
    events: mpsc::Sender<EngineEvent>,
    fault: mpsc::Sender<EngineError>,
    renewals: Arc<Notify>,
    hash: Mutex<Option<integrity::Hasher>>,
}

/// Runs one transfer to completion, or to a resumable stop.
pub async fn transfer(
    engine: Arc<Engine>,
    mut spec: TransferSpec,
    events: mpsc::Sender<EngineEvent>,
    mut control: ControlRx,
) -> Outcome {
    let started = Instant::now();
    let _ = events.send(EngineEvent::Phase(JobState::Probing)).await;

    let probe = match probe::probe(&engine.transport, &spec.envelope).await {
        Ok(p) => p,
        // The link died before a single byte moved. There is a live browser session on the
        // other end of the pipe, so ask it for a fresh URL before troubling the user.
        Err(e) if e.class == Class::Renewable => {
            let hint = RenewalHint {
                url: spec.envelope.effective_url().to_owned(),
                page_url: spec.envelope.page_url.clone(),
                tab_id: spec.envelope.tab_id,
                reason: e.context.clone(),
            };
            let _ = events.send(EngineEvent::UrlExpired(hint)).await;
            let renewed = tokio::select! {
                envelope = control.renew.recv() => envelope,
                _ = tokio::time::sleep(RENEWAL_TIMEOUT) => None,
                _ = control.stop.wait() => return Err(Interrupted::Stopped),
            };
            let Some(envelope) = renewed else {
                return Err(Interrupted::NeedsDecision(Decision::UrlUnrecoverable {
                    page_url: spec.envelope.page_url.clone(),
                }));
            };
            spec.envelope = envelope;
            match probe::probe(&engine.transport, &spec.envelope).await {
                Ok(p) => p,
                Err(e) if e.class == Class::Renewable => {
                    return Err(Interrupted::NeedsDecision(Decision::UrlUnrecoverable {
                        page_url: spec.envelope.page_url.clone(),
                    }))
                }
                Err(e) => return Err(classify_setup_failure(e)),
            }
        }
        Err(e) => return Err(classify_setup_failure(e)),
    };
    let origin = PolicyCache::origin_of(&probe.final_url);
    engine.policy.update(&origin, |p| {
        p.ranges = Some(probe.ranges);
    });
    let _ = events.send(EngineEvent::Probed(Box::new(probe.clone()))).await;

    let filename = spec.filename.clone().unwrap_or_else(|| probe.filename.clone());
    if let Err(e) = std::fs::create_dir_all(&spec.dest_dir) {
        return Err(Interrupted::NeedsDecision(Decision::PermissionDenied {
            path: format!("{} — {e}", spec.dest_dir.display()),
        }));
    }

    match probe.total {
        // The good case: a known size, and ranges we were able to trust.
        Some(total) if total > 0 => {
            run_sized(engine, spec, probe, filename, total, events, &mut control, started).await
        }
        // Unknown size: one stream, written as it arrives (02 §3 — hard gates).
        _ => run_streaming(engine, spec, probe, filename, events, &mut control, started).await,
    }
}

fn classify_setup_failure(e: EngineError) -> Interrupted {
    match e.class {
        Class::Fatal => Interrupted::Failed(e),
        Class::Integrity => Interrupted::NeedsDecision(
            e.decision()
                .unwrap_or(Decision::IntegrityMismatch { detail: e.context }),
        ),
        // Transient, renewable and local setup failures leave nothing behind and are worth
        // another attempt later; the daemon retries a stalled job indefinitely.
        _ => Interrupted::Stopped,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_sized(
    engine: Arc<Engine>,
    spec: TransferSpec,
    probe: Probe,
    filename: String,
    total: u64,
    events: mpsc::Sender<EngineEvent>,
    control: &mut ControlRx,
    started: Instant,
) -> Outcome {
    let (dest, part, meta_path) = resolve_paths(&spec.dest_dir, &filename);
    let _ = events
        .send(EngineEvent::Destination {
            dest: dest.clone(),
            part: part.clone(),
        })
        .await;
    let block_size = block_size_for(total);

    // Resume, or start clean. A `.meta` that disagrees with the server about the file is
    // never merged — that is the one thing a download manager must not do.
    let (meta, bitmap) = match MetaFile::open(&meta_path) {
        Ok((meta, bitmap)) => {
            if let Some(decision) = validators_changed(meta.header(), &probe, total) {
                return Err(Interrupted::NeedsDecision(decision));
            }
            (meta, bitmap)
        }
        Err(_) => {
            let header = header_for(&probe, &spec.envelope, &filename, total, block_size);
            match MetaFile::create(&meta_path, header) {
                Ok(meta) => (meta, roaring::RoaringBitmap::new()),
                Err(e) => return Err(local_decision(e, &part)),
            }
        }
    };

    let file = match std::fs::File::options()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(&part)
    {
        Ok(f) => f,
        Err(e) => return Err(local_decision(e.into(), &part)),
    };
    if let Err(e) = fsalloc::preallocate(&file, total) {
        return Err(local_decision(e.into(), &part));
    }

    let map = BlockMap::from_bitmap(total, block_size, bitmap);
    let already = map.completed_bytes();
    let scheduler = Arc::new(Scheduler::new(map));
    let writer = Arc::new(Writer::spawn(file, spec.write_budget, filename.clone()));

    let origin = PolicyCache::origin_of(&probe.final_url);
    let policy = engine.policy.get(&origin);
    let cap = spec
        .max_connections
        .min(policy.plateau.unwrap_or(u8::MAX))
        .max(1);
    let parallel = probe.mode == TransferMode::Parallel
        && !policy.hostile_to_concurrency
        && cap > 1
        && total >= probe::PARALLEL_FLOOR;
    let start_at = if parallel {
        policy.plateau.unwrap_or(START_CONNECTIONS).min(cap)
    } else {
        1
    };
    if parallel {
        scheduler.seed(start_at);
    }

    let (fault_tx, mut fault_rx) = mpsc::channel(16);
    let job = Arc::new(Job {
        engine: engine.clone(),
        envelope: Mutex::new(spec.envelope.clone()),
        origin: origin.clone(),
        ranges: probe.ranges,
        if_range: (already > 0).then(|| probe.if_range_value()).flatten(),
        part: part.clone(),
        scheduler: scheduler.clone(),
        writer: writer.clone(),
        meta: Arc::new(Mutex::new(meta)),
        addresses: probe.addresses.clone(),
        next_address: AtomicU64::new(0),
        force_h11: policy.prefer_h11.into(),
        protocol: Mutex::new(Some(probe.protocol)),
        retries: (AtomicU32::new(0), AtomicU32::new(0)),
        pushback: false.into(),
        connections: AtomicU8::new(0),
        events: events.clone(),
        fault: fault_tx,
        renewals: Arc::new(Notify::new()),
        // A single stream can hash as it goes; sixteen cannot, so a parallel job verifies
        // by reading the finished file back once.
        hash: Mutex::new(
            (!parallel)
                .then(|| probe.digest.as_ref().map(|d| integrity::Hasher::new(d.algo)))
                .flatten(),
        ),
    });

    let _ = events.send(EngineEvent::Phase(JobState::Downloading)).await;

    let mut controller = Controller::new(cap, start_at);
    let mut workers: std::collections::BTreeMap<Lane, WorkerHandle> = Default::default();
    for lane in 1..=start_at {
        workers.insert(lane, spawn_worker(job.clone(), lane, false));
    }

    let result = supervise(
        &job,
        &mut controller,
        &mut workers,
        &mut fault_rx,
        control,
        parallel,
    )
    .await;

    for handle in workers.values() {
        handle.stop();
    }
    for (lane, handle) in std::mem::take(&mut workers) {
        let _ = handle.join.await;
        job.scheduler.retire(lane);
    }

    // Whatever happened, make what is on disk durable and describable before returning.
    let flush = flush_durably(&job).await;
    if let Err(interrupted) = result {
        let _ = writer.finish().await;
        return Err(interrupted);
    }
    if let Err(e) = flush.and(writer.finish().await) {
        return Err(local_decision(e, &part));
    }

    if let Some(plateau) = controller.learned_plateau() {
        let punished = controller.was_punished();
        engine.policy.update(&origin, |p| {
            p.plateau = Some(plateau);
            p.hostile_to_concurrency = p.hostile_to_concurrency || (punished && plateau <= 1);
        });
    }

    let incremental = job.hash.lock().take().map(|h| h.finish());
    finalize(
        &job,
        &probe,
        &part,
        &dest,
        &meta_path,
        total,
        incremental,
        started,
    )
    .await
}

/// The unknown-size path: one connection, written straight through. Resume still works
/// when the server honours ranges — the journal records whole megabytes only, so a partial
/// block is always re-fetched rather than trusted.
#[allow(clippy::too_many_arguments)]
async fn run_streaming(
    engine: Arc<Engine>,
    spec: TransferSpec,
    probe: Probe,
    filename: String,
    events: mpsc::Sender<EngineEvent>,
    control: &mut ControlRx,
    started: Instant,
) -> Outcome {
    use crate::blocks::DEFAULT_BLOCK;

    let (dest, part, meta_path) = resolve_paths(&spec.dest_dir, &filename);
    let _ = events
        .send(EngineEvent::Destination {
            dest: dest.clone(),
            part: part.clone(),
        })
        .await;

    let (mut meta, bitmap) = match MetaFile::open(&meta_path) {
        Ok(pair) if probe.ranges => pair,
        _ => {
            let header = header_for(&probe, &spec.envelope, &filename, 0, DEFAULT_BLOCK);
            match MetaFile::create(&meta_path, header) {
                Ok(meta) => (meta, roaring::RoaringBitmap::new()),
                Err(e) => return Err(local_decision(e, &part)),
            }
        }
    };
    // Contiguous by construction: block N is only journalled once every byte before it is
    // on the platter.
    let resume_from = bitmap.max().map(|b| (b as u64 + 1) * DEFAULT_BLOCK as u64).unwrap_or(0);

    let file = match std::fs::File::options()
        .create(true)
        .write(true)
        .read(true)
        .truncate(resume_from == 0)
        .open(&part)
    {
        Ok(f) => f,
        Err(e) => return Err(local_decision(e.into(), &part)),
    };
    let writer = Writer::spawn(file, spec.write_budget, filename.clone());

    let _ = events.send(EngineEvent::Phase(JobState::Downloading)).await;

    let key = ClientKey {
        addr: probe.addresses.first().copied(),
        force_h11: false,
    };
    let range = (probe.ranges && resume_from > 0).then(|| ByteRange::new(resume_from, None));
    let response = match engine
        .transport
        .request(&spec.envelope, &probe.final_url, key, range)
    {
        Ok(req) => match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(Interrupted::Stopped.also_log(e)),
        },
        Err(e) => return Err(classify_setup_failure(e)),
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(classify_setup_failure(EngineError::from_status(
            status,
            resume_from > 0,
        )));
    }
    // 200 to a resumed request means the server started over; never stitch.
    let restarted = resume_from > 0 && status != 206;
    if restarted {
        return Err(Interrupted::NeedsDecision(Decision::FileChanged {
            detail: "The file changed on the server.".into(),
        }));
    }

    let mut hasher = probe.digest.as_ref().map(|d| integrity::Hasher::new(d.algo));
    let mut cursor = resume_from;
    let mut journalled = resume_from / DEFAULT_BLOCK as u64;
    let mut stream = response.bytes_stream();
    let mut last_frame = Instant::now();

    loop {
        tokio::select! {
            biased;
            _ = control.stop.wait() => {
                let _ = writer.barrier().await;
                let _ = writer.finish().await;
                return Err(Interrupted::Stopped);
            }
            chunk = stream.next() => {
                let Some(chunk) = chunk else { break };
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => {
                        let _ = writer.barrier().await;
                        let _ = writer.finish().await;
                        return Err(Interrupted::Stopped);
                    }
                };
                engine.limiter.consume(chunk.len() as u64).await;
                if let Some(h) = hasher.as_mut() {
                    h.update(&chunk);
                }
                let len = chunk.len() as u64;
                if let Err(e) = writer.write(cursor, chunk).await {
                    return Err(local_decision(e, &part));
                }
                cursor += len;

                // Data first, then the record of it. The barrier is what makes the
                // journal entry a statement about the platter rather than about a buffer.
                let whole = cursor / DEFAULT_BLOCK as u64;
                if whole > journalled && writer.barrier().await.is_ok() {
                    for block in journalled..whole {
                        let _ = meta.append(block as u32, 1);
                    }
                    let _ = meta.sync();
                    journalled = whole;
                }
                if last_frame.elapsed() >= PROGRESS_INTERVAL {
                    last_frame = Instant::now();
                    let _ = events.try_send(EngineEvent::Progress(ProgressFrame {
                        total: None,
                        completed: cursor,
                        bps: 0,
                        eta_secs: None,
                        connections: 1,
                        block_size: DEFAULT_BLOCK,
                        blocks: 0,
                        runs: Vec::new(),
                        workers: Vec::new(),
                    }));
                }
            }
        }
    }

    if let Err(e) = writer.finish().await {
        return Err(local_decision(e, &part));
    }

    let verified = match (&probe.digest, hasher.map(|h| h.finish())) {
        (Some(expected), Some(actual)) => Some(verification(expected, &actual)),
        _ => None,
    };
    if verified.as_ref().is_some_and(|v| !v.ok) {
        return Err(Interrupted::NeedsDecision(Decision::IntegrityMismatch {
            detail: "The file does not match the checksum the server declared.".into(),
        }));
    }

    let dest = rename_into_place(&part, &dest)?;
    let _ = std::fs::remove_file(&meta_path);
    Ok(Completed {
        path: dest,
        bytes: cursor,
        verified,
        elapsed: started.elapsed(),
        retries: (0, 0),
        protocol: Some(probe.protocol),
        addresses: probe.addresses.len().min(255) as u8,
    })
}

trait AlsoLog {
    fn also_log(self, e: impl std::fmt::Display) -> Self;
}
impl AlsoLog for Interrupted {
    fn also_log(self, e: impl std::fmt::Display) -> Self {
        tracing::debug!(error = %e, "transfer interrupted");
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Supervisor
// ─────────────────────────────────────────────────────────────────────────────

struct WorkerHandle {
    join: tokio::task::JoinHandle<()>,
    stop: Arc<Notify>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
}

impl WorkerHandle {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.stop.notify_waiters();
    }
}

async fn supervise(
    job: &Arc<Job>,
    controller: &mut Controller,
    workers: &mut std::collections::BTreeMap<Lane, WorkerHandle>,
    faults: &mut mpsc::Receiver<EngineError>,
    control: &mut ControlRx,
    parallel: bool,
) -> std::result::Result<(), Interrupted> {
    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_durability = Instant::now();
    let mut last_control = Instant::now();
    let mut last_checkpoint = Instant::now();
    let mut aggregate = crate::ewma::Ewma::three_second();
    // A second, much faster estimate, for the controller only.
    //
    // The displayed rate is smoothed over three seconds so the number on screen does not
    // flicker, and the controller used to read that same estimate — then compare the
    // reading three seconds after a change against the reading before it. One half-life
    // later the estimate is only halfway to the new rate, so a step that truly doubled
    // throughput measured as roughly +50%, and the controller kept concluding that
    // doubling had not paid and walking up one connection at a time instead. Against a
    // per-connection-throttled origin it plateaued around five where it should have
    // reached the ceiling.
    //
    // Three quarters of a second is four half-lives per tick: by the time the controller
    // looks, the estimate is within a few percent of the truth, and the gain it measures
    // is the gain that actually happened.
    let mut control_rate = crate::ewma::Ewma::new(Duration::from_millis(750));
    let mut last_completed = job.scheduler.completed_bytes();

    loop {
        tokio::select! {
            biased;
            _ = control.stop.wait() => return Err(Interrupted::Stopped),
            Some(envelope) = control.renew.recv() => {
                tracing::info!(job = %job.part.display(), "URL renewed by the browser");
                *job.envelope.lock() = envelope;
                job.renewals.notify_waiters();
            }
            Some(fault) = faults.recv() => {
                if let Some(interrupted) = handle_fault(job, fault) {
                    return Err(interrupted);
                }
            }
            _ = ticker.tick() => {}
        }

        if job.scheduler.is_done() {
            return Ok(());
        }

        let now = Instant::now();
        let completed = job.scheduler.completed_bytes();
        let fresh = completed.saturating_sub(last_completed);
        aggregate.observe(fresh, now);
        control_rate.observe(fresh, now);
        last_completed = completed;
        emit_progress(job, &aggregate, now).await;

        if now.duration_since(last_durability) >= DURABILITY_INTERVAL {
            last_durability = now;
            if let Err(e) = flush_durably(job).await {
                return Err(local_decision(e, &job.part));
            }
            let due = now.duration_since(last_checkpoint) >= CHECKPOINT_INTERVAL
                || job.meta.lock().journal_len() > CHECKPOINT_BYTES / 4096;
            if due {
                last_checkpoint = now;
                let bitmap = job.scheduler.bitmap();
                let _ = job.meta.lock().checkpoint(&bitmap);
            }
        }

        if now.duration_since(last_control) < SUPERVISE_INTERVAL {
            continue;
        }
        last_control = now;

        // A struggling origin pauses every job on it together, rather than sixteen workers
        // each hammering it.
        if job.engine.policy.breaker(&job.origin, now) == BreakerState::Open {
            continue;
        }

        if parallel {
            if let Some(lane) = job.scheduler.evict_candidate(now) {
                tracing::debug!(lane, "evicting a worker stuck on a slow path");
                if let Some(handle) = workers.remove(&lane) {
                    handle.stop();
                }
                job.scheduler.retire(lane);
                if let Some(free) = free_lane(workers) {
                    workers.insert(free, spawn_worker(job.clone(), free, false));
                }
            }

            if let Some((victim, range)) = job.scheduler.hedge_candidate(now) {
                if let Some(free) = free_lane(workers) {
                    tracing::debug!(victim, "hedging the straggler");
                    let lease = job.scheduler.install_hedge(free, victim, range);
                    workers.insert(free, spawn_hedge(job.clone(), free, lease));
                }
            }

            let pushback = job.pushback.swap(false, Ordering::SeqCst);
            match controller.tick(control_rate.rate(now), pushback, now) {
                Action::Add => {
                    if let Some(free) = free_lane(workers) {
                        workers.insert(free, spawn_worker(job.clone(), free, false));
                    }
                }
                Action::Remove(n) => {
                    for _ in 0..n {
                        if workers.len() <= 1 {
                            break;
                        }
                        if let Some(lane) = workers.keys().next_back().copied() {
                            if let Some(handle) = workers.remove(&lane) {
                                handle.stop();
                            }
                            job.scheduler.retire(lane);
                        }
                    }
                }
                Action::Hold => {}
            }
        }

        workers.retain(|lane, handle| {
            if handle.join.is_finished() {
                job.scheduler.retire(*lane);
                false
            } else {
                true
            }
        });
        job.connections
            .store(workers.len() as u8, Ordering::Relaxed);

        // Every worker is gone but the file is not finished: rebuild rather than hang.
        if workers.is_empty() {
            if job.scheduler.is_done() {
                return Ok(());
            }
            if let Some(free) = free_lane(workers) {
                workers.insert(free, spawn_worker(job.clone(), free, false));
            }
        }
    }
}

fn handle_fault(job: &Arc<Job>, fault: EngineError) -> Option<Interrupted> {
    match fault.class {
        Class::Fatal => Some(Interrupted::Failed(fault)),
        Class::Integrity => Some(Interrupted::NeedsDecision(
            fault
                .decision()
                .unwrap_or(Decision::IntegrityMismatch { detail: fault.context }),
        )),
        Class::Local => Some(local_decision(fault, &job.part)),
        Class::Renewable => Some(Interrupted::NeedsDecision(Decision::UrlUnrecoverable {
            page_url: job.envelope.lock().page_url.clone(),
        })),
        Class::Transient => {
            job.retries.0.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

fn free_lane(workers: &std::collections::BTreeMap<Lane, WorkerHandle>) -> Option<Lane> {
    (1..=crate::concurrency::HARD_CAP).find(|lane| !workers.contains_key(lane))
}

async fn emit_progress(job: &Arc<Job>, aggregate: &crate::ewma::Ewma, now: Instant) {
    let frame = job.scheduler.frame(now);
    let bps = aggregate.rate(now) as u64;
    let remaining = frame.total.saturating_sub(frame.completed);
    let eta = (bps > 0).then(|| (remaining / bps.max(1)).min(u32::MAX as u64) as u32);
    let _ = job.events.try_send(EngineEvent::Progress(ProgressFrame {
        total: Some(frame.total),
        completed: frame.completed,
        bps,
        eta_secs: eta,
        connections: frame.connections,
        block_size: frame.block_size,
        blocks: frame.blocks,
        runs: frame.runs,
        workers: frame.workers,
    }));
}

/// data written → fsync(data) → append journal record → fsync(meta). Always this order.
async fn flush_durably(job: &Arc<Job>) -> Result<()> {
    let blocks = job.scheduler.take_journal();
    if blocks.is_empty() {
        return Ok(());
    }
    job.writer.barrier().await?;
    let mut meta = job.meta.lock();
    for (start, count) in runs_of(blocks) {
        meta.append(start, count)?;
    }
    meta.sync()
}

fn runs_of(mut blocks: Vec<u32>) -> Vec<(u32, u32)> {
    blocks.sort_unstable();
    blocks.dedup();
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for block in blocks {
        match runs.last_mut() {
            Some((start, count)) if *start + *count == block => *count += 1,
            _ => runs.push((block, 1)),
        }
    }
    runs
}

// ─────────────────────────────────────────────────────────────────────────────
// Workers
// ─────────────────────────────────────────────────────────────────────────────

fn spawn_worker(job: Arc<Job>, lane: Lane, _hedge: bool) -> WorkerHandle {
    let notify = Arc::new(Notify::new());
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Stop {
        notify: notify.clone(),
        flag: flag.clone(),
    };
    let join = tokio::spawn(async move { run_worker(job, lane, stop).await });
    WorkerHandle {
        join,
        stop: notify,
        stopped: flag,
    }
}

fn spawn_hedge(job: Arc<Job>, lane: Lane, lease: Lease) -> WorkerHandle {
    let notify = Arc::new(Notify::new());
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Stop {
        notify: notify.clone(),
        flag: flag.clone(),
    };
    let join = tokio::spawn(async move {
        // One request per contiguous stretch the straggler has not already covered.
        // `fetch_lease` returns early — with the offset to resume from — whenever it finds
        // the block under its cursor already complete, so this loop is how a hedge skips
        // ahead without ever writing a byte at the wrong offset.
        let mut cursor = lease.start;
        while cursor < lease.end() && !stop.is_stopped() {
            match fetch_lease(&job, lane, &lease, cursor, &stop, true).await {
                // A hedge is a bet, not a guarantee: it never retries, because the worker
                // it is racing is still running and will finish the range regardless.
                Err(_) => break,
                Ok(reached) if reached > cursor => cursor = reached,
                Ok(_) => break,
            }
        }
        job.scheduler.retire(lane);
    });
    WorkerHandle {
        join,
        stop: notify,
        stopped: flag,
    }
}

async fn run_worker(job: Arc<Job>, lane: Lane, stop: Stop) {
    let mut attempt = 0u32;
    while !stop.is_stopped() {
        let Some(lease) = job.scheduler.acquire(lane, Instant::now()) else {
            break;
        };
        let mut cursor = lease.start;
        loop {
            if stop.is_stopped() {
                job.scheduler.release(lane);
                return;
            }
            match fetch_lease(&job, lane, &lease, cursor, &stop, false).await {
                Ok(_reached) => {
                    if attempt > 0 {
                        job.retries.1.fetch_add(1, Ordering::Relaxed);
                    }
                    break;
                }
                Err((e, reached)) => {
                    cursor = reached;
                    match e.class {
                        Class::Transient => {
                            job.engine
                                .policy
                                .record_transient_failure(&job.origin, Instant::now());
                            job.retries.0.fetch_add(1, Ordering::Relaxed);
                            attempt += 1;
                            if attempt > WORKER_ATTEMPTS {
                                // Give the lease back rather than sit on it. Another
                                // worker — on a different address — will pick it up.
                                job.scheduler.release(lane);
                                let _ = job.fault.send(e).await;
                                return;
                            }
                            tokio::select! {
                                _ = tokio::time::sleep(backoff(attempt)) => {}
                                _ = stop.wait() => { job.scheduler.release(lane); return }
                            }
                        }
                        Class::Renewable => {
                            if !await_renewal(&job, &stop).await {
                                job.scheduler.release(lane);
                                let _ = job.fault.send(e).await;
                                return;
                            }
                        }
                        _ => {
                            job.scheduler.release(lane);
                            let _ = job.fault.send(e).await;
                            return;
                        }
                    }
                }
            }
        }
        job.scheduler.release(lane);
    }
    job.scheduler.release(lane);
}

/// Asks the browser for a fresh URL and waits for it. Only the first worker to notice
/// actually emits the event; the rest ride along on the same renewal.
async fn await_renewal(job: &Arc<Job>, stop: &Stop) -> bool {
    let before = job.envelope.lock().effective_url().to_owned();
    let notified = job.renewals.notified();
    let hint = {
        let envelope = job.envelope.lock();
        RenewalHint {
            url: before.clone(),
            page_url: envelope.page_url.clone(),
            tab_id: envelope.tab_id,
            reason: "the signed URL expired".into(),
        }
    };
    let _ = job.events.send(EngineEvent::UrlExpired(hint)).await;

    tokio::select! {
        _ = notified => job.envelope.lock().effective_url() != before,
        _ = tokio::time::sleep(RENEWAL_TIMEOUT) => false,
        _ = stop.wait() => false,
    }
}

/// Full jitter, capped. A synchronised retry storm is worse than a slow retry.
fn backoff(attempt: u32) -> Duration {
    use rand::Rng as _;
    let ceiling = Duration::from_millis(250 * 2u64.saturating_pow(attempt.min(8)));
    let ceiling = ceiling.min(Duration::from_secs(30));
    Duration::from_secs_f64(rand::rng().random_range(0.0..=ceiling.as_secs_f64()))
}

/// Streams one lease. Returns the cursor it reached, whether it finished or failed — the
/// caller resumes from there, never from the start.
async fn fetch_lease(
    job: &Arc<Job>,
    lane: Lane,
    lease: &Lease,
    from: u64,
    stop: &Stop,
    hedge: bool,
) -> std::result::Result<u64, (EngineError, u64)> {
    let mut cursor = from;
    if cursor >= lease.end() {
        return Ok(cursor);
    }

    let envelope = job.envelope.lock().clone();
    let url = envelope.effective_url().to_owned();
    let key = ClientKey {
        addr: job.address_for(lane),
        force_h11: job.force_h11.load(Ordering::Relaxed),
    };
    let range = job
        .ranges
        .then(|| ByteRange::new(cursor, Some(lease.end())));

    let mut builder = match job.engine.transport.request(&envelope, &url, key, range) {
        Ok(b) => b,
        Err(e) => return Err((e, cursor)),
    };
    // Revalidate on resume: a 200 where a 206 belongs means the file changed, and merging
    // bytes from two versions is the single worst thing a download manager can do.
    if let Some(validator) = &job.if_range {
        if cursor > 0 || from > 0 {
            builder = builder.header("If-Range", validator);
        }
    }

    let response = match builder.send().await {
        Ok(r) => r,
        Err(e) => return Err((EngineError::from(e), cursor)),
    };
    let status = response.status().as_u16();
    if status == 429 || status == 503 {
        job.pushback.store(true, Ordering::SeqCst);
    }
    if !(200..300).contains(&status) {
        return Err((EngineError::from_status(status, true), cursor));
    }
    *job.protocol.lock() = Some(transport::protocol_of(response.version()));

    if job.ranges {
        match status {
            206 => {
                let claimed = response
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .and_then(transport::parse_content_range);
                match claimed {
                    Some(cr) if cr.start == cursor => {}
                    Some(cr) => {
                        return Err((
                            EngineError::integrity(format!(
                                "the server answered a request for byte {cursor} with byte {}",
                                cr.start
                            )),
                            cursor,
                        ))
                    }
                    None => {
                        return Err((
                            EngineError::integrity("the server sent a 206 with no Content-Range"),
                            cursor,
                        ))
                    }
                }
            }
            _ if cursor > 0 => {
                return Err((
                    EngineError::integrity("The file changed on the server."),
                    cursor,
                ))
            }
            _ => {}
        }
    }
    // A `Content-Encoding` on a ranged response destroys byte offsets. We asked for
    // identity; if the server ignored that, the bytes cannot be placed.
    if let Some(encoding) = response.headers().get("content-encoding") {
        if !encoding.as_bytes().eq_ignore_ascii_case(b"identity") {
            return Err((
                EngineError::integrity("the server compressed a ranged response"),
                cursor,
            ));
        }
    }

    job.engine.policy.record_success(&job.origin);
    let block = job.block_size();
    let mut stream = response.bytes_stream();

    loop {
        let end = lease.end();
        if cursor >= end {
            return Ok(cursor);
        }
        // The straggler got here first. Skipping ahead is right — there is no point
        // spending bandwidth on a block that is already on disk — but it cannot be done
        // by moving the cursor, because this response body is positioned at the *old*
        // offset. Advancing one without the other writes every subsequent chunk `skipped`
        // bytes past where it belongs, and produces a full-length file of the wrong
        // content. Hand the new offset back instead, and let the caller open a request
        // that actually starts there.
        if hedge && job.scheduler.is_block_complete((cursor / block) as u32) {
            return Ok(((cursor / block + 1) * block).min(end));
        }

        let chunk = tokio::select! {
            biased;
            _ = stop.wait() => return Ok(cursor),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            // The stream ended early. Whatever landed is durable; the caller retries the
            // remainder rather than the whole lease.
            return if cursor >= lease.end() {
                Ok(cursor)
            } else {
                Err((
                    EngineError::transient("the connection closed early"),
                    cursor,
                ))
            };
        };
        let mut chunk = match chunk {
            Ok(c) => c,
            Err(e) => return Err((EngineError::from(e), cursor)),
        };
        // Servers overshoot a range surprisingly often; trim rather than write past it.
        let room = (end - cursor) as usize;
        if chunk.len() > room {
            chunk.truncate(room);
        }
        let len = chunk.len() as u64;
        if len == 0 {
            continue;
        }

        job.engine.limiter.consume(len).await;
        if let Some(h) = job.hash.lock().as_mut() {
            h.update(&chunk);
        }
        if let Err(e) = job.writer.write(cursor, chunk).await {
            return Err((e, cursor));
        }
        cursor += len;
        job.scheduler.progress(lane, cursor, len, Instant::now());
    }
}

impl Job {
    /// Hands each worker a distinct resolved address, so eviction always reconnects
    /// somewhere else.
    fn address_for(&self, _lane: Lane) -> Option<SocketAddr> {
        if self.addresses.is_empty() {
            return None;
        }
        let index = self.next_address.fetch_add(1, Ordering::Relaxed) as usize;
        Some(self.addresses[index % self.addresses.len()])
    }

    fn block_size(&self) -> u64 {
        self.meta.lock().header().block_size as u64
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Finalize
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn finalize(
    job: &Arc<Job>,
    probe: &Probe,
    part: &Path,
    dest: &Path,
    meta_path: &Path,
    total: u64,
    incremental: Option<String>,
    started: Instant,
) -> Outcome {
    let on_disk = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if on_disk != total {
        return Err(Interrupted::NeedsDecision(Decision::IntegrityMismatch {
            detail: format!("The file is {on_disk} bytes; the server said {total}."),
        }));
    }

    let verified = match &probe.digest {
        Some(expected) => {
            let actual = match incremental {
                Some(hex) => hex,
                None => match integrity::hash_file(part, expected.algo).await {
                    Ok(hex) => hex,
                    Err(e) => return Err(local_decision(e, part)),
                },
            };
            Some(verification(expected, &actual))
        }
        None => None,
    };
    if verified.as_ref().is_some_and(|v| !v.ok) {
        return Err(Interrupted::NeedsDecision(Decision::IntegrityMismatch {
            detail: "The file does not match the checksum the server declared.".into(),
        }));
    }

    // The final filename never exists until the file is complete and verified.
    let dest = rename_into_place(part, dest)?;
    let _ = std::fs::remove_file(meta_path);

    Ok(Completed {
        path: dest,
        bytes: total,
        verified,
        elapsed: started.elapsed(),
        retries: (
            job.retries.0.load(Ordering::Relaxed),
            job.retries.1.load(Ordering::Relaxed),
        ),
        protocol: *job.protocol.lock(),
        addresses: job.addresses.len().min(255) as u8,
    })
}

fn verification(expected: &Expected, actual: &str) -> Verification {
    Verification {
        algorithm: expected.algo.label().to_owned(),
        ok: expected.hex.eq_ignore_ascii_case(actual),
    }
}

fn rename_into_place(part: &Path, dest: &Path) -> std::result::Result<PathBuf, Interrupted> {
    match std::fs::rename(part, dest) {
        Ok(()) => Ok(dest.to_owned()),
        Err(_) => {
            // Something claimed the name while we were downloading. Take the next free one
            // rather than clobber it.
            let dir = dest.parent().unwrap_or(Path::new("."));
            let name = dest
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download");
            let alternative = naming::unique_path(dir, name);
            std::fs::rename(part, &alternative)
                .map(|_| alternative)
                .map_err(|e| {
                    Interrupted::NeedsDecision(Decision::PermissionDenied {
                        path: format!("{} — {e}", dest.display()),
                    })
                })
        }
    }
}

fn local_decision(e: EngineError, part: &Path) -> Interrupted {
    let full = e
        .source
        .as_ref()
        .and_then(|s| s.downcast_ref::<std::io::Error>())
        .is_some_and(crate::error::is_disk_full)
        || e.context.to_ascii_lowercase().contains("disk");
    if full {
        let drive = part
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        return Interrupted::NeedsDecision(Decision::DiskFull { needed: 0, drive });
    }
    Interrupted::NeedsDecision(Decision::PermissionDenied {
        path: format!("{} — {}", part.display(), e.context),
    })
}

/// Where this job's three files live.
///
/// Resuming has to find the partial it left behind, so an existing `.vxpart.meta` for the
/// obvious name wins over picking a fresh ` (2)`. Only a genuinely new download dedupes.
fn resolve_paths(dir: &Path, filename: &str) -> (PathBuf, PathBuf, PathBuf) {
    let direct = dir.join(naming::sanitize(filename));
    let part = with_suffix(&direct, ".vxpart");
    let meta = MetaFile::meta_path(&part);
    if meta.exists() {
        return (direct, part, meta);
    }
    let dest = naming::unique_path(dir, filename);
    let part = with_suffix(&dest, ".vxpart");
    let meta = MetaFile::meta_path(&part);
    (dest, part, meta)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Request headers worth writing next to the file. A `.meta` sidecar is the only thing
/// needed to rebuild a job after `vortex.db` is lost, and a resume without `Referer` is the
/// single most common cause of a 403 on the second attempt. Credentials are never in this
/// list: `Cookie` and `Authorization` live in memory for the life of the job and nowhere
/// else (01 §Security boundaries).
const PERSISTED_HEADERS: [&str; 7] = [
    "user-agent",
    "referer",
    "origin",
    "accept",
    "accept-language",
    "sec-fetch-site",
    "sec-fetch-mode",
];

fn header_for(
    probe: &Probe,
    envelope: &RequestEnvelope,
    filename: &str,
    total: u64,
    block_size: u32,
) -> MetaHeader {
    MetaHeader {
        origin_url: probe.url.clone(),
        final_url: probe.final_url.clone(),
        method: "GET".into(),
        headers: envelope
            .headers
            .iter()
            .filter(|(k, _)| PERSISTED_HEADERS.iter().any(|p| k.eq_ignore_ascii_case(p)))
            .cloned()
            .collect(),
        page_url: envelope.page_url.clone(),
        filename: filename.to_owned(),
        total,
        block_size,
        etag: probe.etag.clone(),
        weak_etag: probe.weak_etag,
        last_modified: probe.last_modified.clone(),
        content_type: probe.content_type.clone(),
        digest: probe
            .digest
            .as_ref()
            .map(|d| (algo_name(d.algo).to_owned(), d.hex.clone())),
        created_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

fn algo_name(algo: Algo) -> &'static str {
    algo.label()
}

/// A resumed job whose validators moved is not the same file. Never merge.
fn validators_changed(header: &MetaHeader, probe: &Probe, total: u64) -> Option<Decision> {
    if header.total != 0 && header.total != total {
        return Some(Decision::FileChanged {
            detail: "The file changed on the server.".into(),
        });
    }
    match (&header.etag, &probe.etag) {
        (Some(before), Some(now)) if before != now => Some(Decision::FileChanged {
            detail: "The file changed on the server.".into(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_runs_collapse_adjacent_blocks() {
        assert_eq!(runs_of(vec![3, 1, 2, 7]), vec![(1, 3), (7, 1)]);
        assert_eq!(runs_of(vec![5, 5, 5]), vec![(5, 1)]);
        assert!(runs_of(vec![]).is_empty());
    }

    #[test]
    fn backoff_is_jittered_and_capped() {
        for attempt in 1..12 {
            let d = backoff(attempt);
            assert!(d <= Duration::from_secs(30), "attempt {attempt} gave {d:?}");
        }
    }

    #[test]
    fn a_changed_etag_stops_the_resume_rather_than_merging() {
        let probe = Probe {
            url: "u".into(),
            final_url: "u".into(),
            host: "h".into(),
            total: Some(100),
            ranges: true,
            mode: TransferMode::Parallel,
            etag: Some("\"new\"".into()),
            weak_etag: false,
            last_modified: None,
            content_type: None,
            filename: "f".into(),
            category: vortex_proto::Category::Other,
            digest: None,
            protocol: Protocol::H2,
            addresses: vec![],
            ranges_distrusted: false,
        };
        let mut header = header_for(&probe, &RequestEnvelope::new("u"), "f", 100, 1 << 20);
        header.etag = Some("\"old\"".into());
        assert!(matches!(
            validators_changed(&header, &probe, 100),
            Some(Decision::FileChanged { .. })
        ));

        header.etag = Some("\"new\"".into());
        assert!(validators_changed(&header, &probe, 100).is_none());
        assert!(validators_changed(&header, &probe, 200).is_some());
    }
}
