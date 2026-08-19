//! The media job: the same contract as a file transfer, a different middle.
//!
//! [`transfer`] has the signature of [`vortex_engine::job::transfer`] and returns the same
//! [`Outcome`], so the daemon starts it, pauses it, resumes it and reports it through
//! exactly the machinery that already exists. What differs is what happens between
//! `Probing` and the rename: a manifest is resolved, tracks are assembled, and ffmpeg
//! combines them.
//!
//! The working directory *is* the `.vxpart` — a directory rather than a file. That keeps
//! every partial artefact under one path the owner already knows how to clean up, and it
//! is what makes `Retry mux` free: the segments are still there, so a re-run walks
//! straight past the download and back into ffmpeg (04 §7).

use crate::plan::{Container, Plan};
use crate::{fetch, mux, plan, Error};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use vortex_engine::error::EngineError;
use vortex_engine::ewma::Ewma;
use vortex_engine::job::{Completed, ControlRx, Engine, EngineEvent, Interrupted, Outcome};
use vortex_engine::naming;
use vortex_proto::{
    ContainerPreference, Decision, JobState, MediaSelection, MediaSummary, ProgressFrame,
    RenewalHint, RequestEnvelope,
};

/// How long the job waits for the extension to re-mint a URL before asking the user. Same
/// budget as a file transfer, for the same reason: a browser that is going to answer
/// answers in seconds.
const RENEWAL_TIMEOUT: Duration = Duration::from_secs(120);
/// A run gives the extension this many chances to renew before it stops asking. Without a
/// bound, a site that 403s every segment would loop forever.
const RENEWAL_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
pub struct MediaSpec {
    pub envelope: RequestEnvelope,
    pub selection: MediaSelection,
    pub dest_dir: PathBuf,
    /// Already chosen by the user, or `None` to take the title from the manifest.
    pub filename: Option<String>,
    pub max_connections: u8,
    pub container: ContainerPreference,
}

/// Runs one stream to a playable file, or to a resumable stop.
pub async fn transfer(
    engine: Arc<Engine>,
    spec: MediaSpec,
    events: mpsc::Sender<EngineEvent>,
    control: ControlRx,
) -> Outcome {
    run(engine, spec, events, control, 0).await
}

/// Boxed because a renewal restarts the run, and a restart is honestly recursive: a fresh
/// session means a fresh manifest, which means a fresh plan. `renewals` carries across so
/// a site that 403s every request cannot loop forever.
fn run(
    engine: Arc<Engine>,
    spec: MediaSpec,
    events: mpsc::Sender<EngineEvent>,
    control: ControlRx,
    renewals: u32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Outcome> + Send>> {
    Box::pin(one_run(engine, spec, events, control, renewals))
}

async fn one_run(
    engine: Arc<Engine>,
    mut spec: MediaSpec,
    events: mpsc::Sender<EngineEvent>,
    mut control: ControlRx,
    renewals: u32,
) -> Outcome {
    let started = Instant::now();
    let _ = events.send(EngineEvent::Phase(JobState::Probing)).await;

    let plan = resolve(&engine, &mut spec, &events, &mut control).await?;

    if plan.live {
        // A live playlist has no end, so there is nothing to mux and no size to report.
        // Capturing one is a different job shape and is not what this build offers.
        return Err(Interrupted::Failed(EngineError::refusal(
            "That's a live stream. Vortex can't save one yet.",
        )));
    }

    let filename = with_extension(
        spec.filename.as_deref().unwrap_or(&plan.title),
        plan.container,
    );
    if let Err(e) = std::fs::create_dir_all(&spec.dest_dir) {
        return Err(Interrupted::NeedsDecision(Decision::PermissionDenied {
            path: format!("{} — {e}", spec.dest_dir.display()),
        }));
    }
    let (dest, work) = resolve_paths(&spec.dest_dir, &filename);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return Err(Interrupted::NeedsDecision(Decision::PermissionDenied {
            path: format!("{} — {e}", work.display()),
        }));
    }
    let _ = events
        .send(EngineEvent::Destination {
            dest: dest.clone(),
            part: work.clone(),
        })
        .await;

    let _ = events.send(EngineEvent::Phase(JobState::Downloading)).await;
    let mut totals = Totals::new(&plan);
    let mut expired = None;

    for track in &plan.tracks {
        let path = work.join(&track.file);
        let mut reporter = Reporter::new(&events, &plan, totals);
        let outcome = fetch::track(
            &engine,
            &spec.envelope,
            track,
            &path,
            spec.max_connections,
            &control,
            &mut |stats| reporter.observe(stats),
        )
        .await;
        match outcome {
            Ok(stats) => totals.finish(stats),
            Err(fetch::Interruption::Stopped) => return Err(Interrupted::Stopped),
            Err(fetch::Interruption::Local(e)) => return Err(local_decision(e, &work)),
            Err(fetch::Interruption::UrlExpired(reason)) => {
                expired = Some(reason);
                break;
            }
        }
    }

    if let Some(reason) = expired {
        if renewals >= RENEWAL_ATTEMPTS {
            return Err(Interrupted::NeedsDecision(Decision::UrlUnrecoverable {
                page_url: spec.envelope.page_url.clone(),
            }));
        }
        // Segment URLs are minted by the manifest, so a renewed session means a re-resolved
        // plan. Everything already on disk stays: the fetcher counts segments, and a VOD
        // manifest enumerates the same ones every time.
        renew(&mut spec, &events, &mut control, &reason).await?;
        return run(engine, spec, events, control, renewals + 1).await;
    }

    let _ = events.send(EngineEvent::Phase(JobState::Muxing)).await;
    let Some(ffmpeg) = mux::Ffmpeg::find() else {
        return Err(Interrupted::NeedsDecision(Decision::MuxFailed {
            detail: mux::MuxError::Missing.to_string(),
        }));
    };
    let output = work.join(format!("output.{}", plan.container.extension()));
    let summary = totals.summary(&plan);
    let muxed = {
        let mut percent = |value: u8| {
            let mut summary = summary.clone();
            summary.mux_percent = Some(value);
            let _ = events.try_send(EngineEvent::Media(summary));
        };
        mux::run(&ffmpeg, &plan, &work, &output, &control, &mut percent).await
    };
    if let Err(e) = muxed {
        return match e {
            mux::MuxError::Stopped => Err(Interrupted::Stopped),
            // The segments are still on disk, so this is a question, not a loss: the row
            // offers `Retry mux` and a re-run walks straight back to ffmpeg.
            other => Err(Interrupted::NeedsDecision(Decision::MuxFailed {
                detail: other.to_string(),
            })),
        };
    }

    let bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        return Err(Interrupted::NeedsDecision(Decision::MuxFailed {
            detail: "ffmpeg produced an empty file.".to_owned(),
        }));
    }
    if let Err(e) = std::fs::rename(&output, &dest) {
        return Err(local_decision(e.into(), &dest));
    }
    // Only now are the segments expendable (04 — the file has to exist first).
    let _ = std::fs::remove_dir_all(&work);

    // The last word on the job: every segment accounted for, and the muxer finished.
    let mut summary = totals.summary(&plan);
    summary.mux_percent = Some(100);
    let _ = events.send(EngineEvent::Media(summary)).await;
    Ok(Completed {
        path: dest,
        bytes,
        verified: None,
        elapsed: started.elapsed(),
        retries: (totals.missing, 0),
        protocol: None,
        addresses: 1,
    })
}

async fn resolve(
    engine: &Arc<Engine>,
    spec: &mut MediaSpec,
    events: &mpsc::Sender<EngineEvent>,
    control: &mut ControlRx,
) -> Result<Plan, Interrupted> {
    match plan::resolve(engine, &spec.envelope, &spec.selection, spec.container).await {
        Ok(plan) => Ok(plan),
        Err(Error::Refused(refusal)) => Err(Interrupted::Failed(EngineError::refusal(refusal.message))),
        Err(Error::Unsupported(unsupported)) => Err(Interrupted::Failed(EngineError::refusal(
            unsupported.message,
        ))),
        Err(Error::Unreadable(message)) => Err(Interrupted::Failed(EngineError::refusal(message))),
        Err(Error::Transfer(e)) => match e.class {
            vortex_engine::Class::Renewable => {
                renew(spec, events, control, &e.context).await?;
                plan::resolve(engine, &spec.envelope, &spec.selection, spec.container)
                    .await
                    .map_err(|e| match e {
                        Error::Refused(r) => Interrupted::Failed(EngineError::refusal(r.message)),
                        other => {
                            tracing::debug!("the manifest is still unreachable: {other}");
                            Interrupted::Stopped
                        }
                    })
            }
            vortex_engine::Class::Fatal => Err(Interrupted::Failed(e)),
            // Transient and local setup failures leave nothing behind; the daemon retries.
            _ => Err(Interrupted::Stopped),
        },
    }
}

/// Asks the extension for a fresh URL and waits for it, replacing the envelope in place.
async fn renew(
    spec: &mut MediaSpec,
    events: &mpsc::Sender<EngineEvent>,
    control: &mut ControlRx,
    reason: &str,
) -> Result<(), Interrupted> {
    let hint = RenewalHint {
        url: spec.selection.manifest_url.clone(),
        page_url: spec.envelope.page_url.clone(),
        tab_id: spec.envelope.tab_id,
        reason: reason.to_owned(),
    };
    let _ = events.send(EngineEvent::UrlExpired(hint)).await;

    let renewed = tokio::time::timeout(RENEWAL_TIMEOUT, control.renewal())
        .await
        .ok()
        .flatten();
    if control.is_stopped() {
        return Err(Interrupted::Stopped);
    }
    let Some(envelope) = renewed else {
        return Err(Interrupted::NeedsDecision(Decision::UrlUnrecoverable {
            page_url: spec.envelope.page_url.clone(),
        }));
    };
    // The manifest URL travels with the envelope: a re-minted session usually hands back a
    // new signed manifest, and keeping the dead one would renew nothing.
    if !envelope.effective_url().is_empty() {
        spec.selection.manifest_url = envelope.effective_url().to_owned();
    }
    spec.envelope = envelope;
    Ok(())
}

fn local_decision(e: EngineError, path: &Path) -> Interrupted {
    let text = e.context.to_ascii_lowercase();
    if text.contains("disk is full") {
        return Interrupted::NeedsDecision(Decision::DiskFull {
            needed: 0,
            drive: path.to_string_lossy().into_owned(),
        });
    }
    Interrupted::NeedsDecision(Decision::PermissionDenied {
        path: format!("{} — {}", path.display(), e.context),
    })
}

/// Where this job's file and its segments live.
///
/// An existing work directory is *reclaimed* by exact name, exactly as a file transfer
/// reclaims a `.vxpart`: without that, the second run of a paused job would see the first
/// run's partial, decide the name was taken, and start over as `Colour Bars (2).mp4`.
fn resolve_paths(dir: &Path, filename: &str) -> (PathBuf, PathBuf) {
    let direct = dir.join(naming::sanitize(filename));
    let work = work_dir(&direct);
    if work.is_dir() {
        return (direct, work);
    }
    let dest = naming::unique_path(dir, filename);
    let work = work_dir(&dest);
    (dest, work)
}

/// `<destination>.vxpart`, as a directory. One path to clean up, and one path that means
/// "this job is not finished".
pub fn work_dir(dest: &Path) -> PathBuf {
    let mut path = dest.as_os_str().to_owned();
    path.push(".vxpart");
    PathBuf::from(path)
}

/// The container decides the extension, whatever the title happened to end with.
fn with_extension(filename: &str, container: Container) -> String {
    let stem = match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 4 => stem,
        _ => filename,
    };
    format!("{}.{}", naming::sanitize(stem), container.extension())
}

// ─────────────────────────────────────────────────────────────────────────────
// Progress
// ─────────────────────────────────────────────────────────────────────────────

/// Running counts across every track. Tracks are fetched one after another, so "done" is a
/// prefix — which is exactly what the segment map needs to draw.
#[derive(Debug, Clone, Copy)]
struct Totals {
    done: u32,
    total: u32,
    missing: u32,
    bytes: u64,
    /// Bytes and segments completed by tracks that have already finished.
    settled_bytes: u64,
    settled_done: u32,
}

impl Totals {
    fn new(plan: &Plan) -> Self {
        Self {
            done: 0,
            total: plan.segments(),
            missing: 0,
            bytes: 0,
            settled_bytes: 0,
            settled_done: 0,
        }
    }

    fn observe(&mut self, stats: fetch::Stats) {
        self.done = self.settled_done + stats.done;
        self.bytes = self.settled_bytes + stats.bytes;
        self.missing = stats.missing;
    }

    fn finish(&mut self, stats: fetch::Stats) {
        self.settled_done += stats.done;
        self.settled_bytes += stats.bytes;
        self.done = self.settled_done;
        self.bytes = self.settled_bytes;
    }

    fn summary(&self, plan: &Plan) -> MediaSummary {
        MediaSummary {
            segments_total: self.total,
            segments_done: self.done,
            segments_missing: self.missing,
            mux_percent: None,
            container_note: plan.container_note.clone(),
        }
    }
}

/// Turns segment counts into the frames the rest of the system already speaks.
struct Reporter<'a> {
    events: &'a mpsc::Sender<EngineEvent>,
    totals: Totals,
    estimated: Option<u64>,
    note: Option<String>,
    rate: Ewma,
}

impl<'a> Reporter<'a> {
    fn new(events: &'a mpsc::Sender<EngineEvent>, plan: &Plan, totals: Totals) -> Self {
        Self {
            events,
            totals,
            estimated: plan.estimated_bytes(),
            note: plan.container_note.clone(),
            rate: Ewma::three_second(),
        }
    }

    /// How large the finished file will be.
    ///
    /// The manifest's `bandwidth × duration / 8` is a *peak* declaration and routinely
    /// lands well high, so it is only used until real segments have arrived. After that,
    /// bytes-so-far scaled by segments-remaining is a far better predictor, and it lands
    /// on the truth exactly when the last segment does — which is the difference between
    /// a bar that reaches 100% and one that stops at 31%.
    ///
    /// Tracks are fetched one after another and audio is cheaper per segment than video,
    /// so the projection runs a little high while the video track is still going. It is
    /// still an order of magnitude closer than the declaration it replaces.
    fn projected(&self) -> Option<u64> {
        if self.totals.done > 0 && self.totals.total > 0 {
            let scaled =
                self.totals.bytes as u128 * self.totals.total as u128 / self.totals.done as u128;
            return Some((scaled as u64).max(self.totals.bytes));
        }
        self.estimated.map(|estimate| estimate.max(self.totals.bytes))
    }

    fn observe(&mut self, stats: fetch::Stats) {
        let before = self.totals.bytes;
        self.totals.observe(stats);
        let now = Instant::now();
        self.rate.observe(self.totals.bytes.saturating_sub(before), now);

        let bps = self.rate.rate(now) as u64;
        let total = self.projected();
        let frame = ProgressFrame {
            total,
            completed: self.totals.bytes,
            bps,
            eta_secs: eta(total, self.totals.bytes, bps),
            connections: fetch::START_CONNECTIONS,
            block_size: block_size(self.estimated, self.totals.total),
            blocks: self.totals.total,
            runs: runs(self.totals.done, self.totals.total),
            workers: Vec::new(),
        };
        let _ = self.events.try_send(EngineEvent::Progress(frame));
        let _ = self.events.try_send(EngineEvent::Media(MediaSummary {
            segments_total: self.totals.total,
            segments_done: self.totals.done,
            segments_missing: self.totals.missing,
            mux_percent: None,
            container_note: self.note.clone(),
        }));
    }
}

/// Completed segments are a prefix, so the map is two runs: done, then pending.
fn runs(done: u32, total: u32) -> Vec<(u32, u32, u8)> {
    let mut runs = Vec::with_capacity(2);
    if done > 0 {
        runs.push((0, done.min(total), 0));
    }
    if total > done {
        runs.push((done, total - done, 1));
    }
    runs
}

fn block_size(estimated: Option<u64>, segments: u32) -> u32 {
    match (estimated, segments) {
        (Some(bytes), count) if count > 0 => (bytes / count as u64).min(u32::MAX as u64) as u32,
        _ => 0,
    }
}

fn eta(total: Option<u64>, completed: u64, bps: u64) -> Option<u32> {
    let remaining = total?.saturating_sub(completed);
    (bps > 0).then(|| (remaining / bps).min(u32::MAX as u64) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_decides_the_extension() {
        assert_eq!(with_extension("A Lecture.m3u8", Container::Mp4), "A Lecture.mp4");
        assert_eq!(with_extension("A Lecture", Container::Mkv), "A Lecture.mkv");
        assert_eq!(
            with_extension("Q3 2024. Results", Container::Mp4),
            "Q3 2024. Results.mp4",
            "a dot inside a title is not an extension"
        );
    }

    #[test]
    fn the_work_directory_is_the_part_path() {
        let dest = Path::new("/downloads/A Lecture.mp4");
        assert_eq!(work_dir(dest), Path::new("/downloads/A Lecture.mp4.vxpart"));
    }

    #[test]
    fn the_segment_map_is_a_prefix_and_a_remainder() {
        assert_eq!(runs(0, 100), vec![(0, 100, 1)]);
        assert_eq!(runs(40, 100), vec![(0, 40, 0), (40, 60, 1)]);
        assert_eq!(runs(100, 100), vec![(0, 100, 0)]);
    }

    #[test]
    fn the_estimate_gives_way_to_the_truth() {
        // 1 GB declared, 1.2 GB actually arriving: the bar must not sit at 100% and lie.
        assert_eq!(block_size(Some(1_000_000), 100), 10_000);
        assert_eq!(block_size(None, 100), 0);
        assert_eq!(eta(Some(1000), 400, 100), Some(6));
        assert_eq!(eta(Some(1000), 400, 0), None);
    }
}
