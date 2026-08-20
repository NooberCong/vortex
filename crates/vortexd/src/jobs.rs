//! The runtime job record and the small decisions that surround one.
//!
//! The state machine itself lives in [`crate::daemon`]; what is here is the arithmetic it
//! needs — where a job should land, what it is called before anyone has asked the server,
//! and how long to wait before trying a stalled transfer again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use vortex_engine::job::Control;
use vortex_engine::meta::MetaFile;
use vortex_engine::naming;
use vortex_proto::{
    Category, JobId, JobSpec, JobState, JobView, Priority, ProgressFrame, TransferMode,
};

pub struct Job {
    pub view: JobView,
    /// Carries live credentials for as long as the job exists in memory. The persisted
    /// copy is redacted (see [`crate::store::redact`]).
    pub spec: JobSpec,
    pub dest_dir: PathBuf,
    /// The `.vxpart` this job owns, once the engine has told us. Before the first run it
    /// is the name the engine *would* choose, which is enough to clean up after a cancel.
    pub part: PathBuf,
    /// The handle to the transfer in flight. A run is always ended by asking it to stop,
    /// never by aborting the task: everything durable is written in order, and a task
    /// dropped mid-write is the one shape of shutdown the ordering cannot help with.
    pub run: Option<Control>,
    pub intent: Intent,
    /// Consecutive stalls. Reset by any progress, and by the user asking for a retry.
    pub attempts: u32,
    pub retry_at: Option<Instant>,
    pub latest: ProgressFrame,
}

/// Why the current run is being stopped. A transfer stops the same way whatever the
/// reason — everything durable is already on disk — so the reason only decides what
/// happens *after* it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Run,
    Pause,
    /// Stop, discard the partial data, and take the row out of the list.
    Cancel,
    Remove { delete_file: bool },
}

impl Job {
    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Stops the current run, if any, and records why.
    pub fn stop(&mut self, intent: Intent) {
        self.intent = intent;
        self.retry_at = None;
        if let Some(control) = &self.run {
            control.stop();
        }
    }
}

/// Deletes a job's partial work: the `.vxpart` and the sidecar beside it.
///
/// A file transfer's partial is a file and a media job's is a directory of segments. One
/// call takes either, because every caller means "the unfinished work, whatever shape it
/// has" — and remembering which shape a job had is exactly the bookkeeping that once let
/// `Start over` on a stream keep the segments it was meant to throw away.
pub fn discard_part(part: &Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_dir_all(part);
    let _ = std::fs::remove_file(MetaFile::meta_path(part));
}

/// The record as it looks the moment a job is submitted — before anyone has asked the
/// server anything. Every field here is a guess the probe will improve on, except the
/// category, which stays put because it already decided which folder the file is in.
pub fn new_view(id: JobId, spec: &JobSpec, dest_dir: &Path, filename: &str) -> JobView {
    JobView {
        id,
        filename: filename.to_owned(),
        dest_path: dest_dir.join(filename).to_string_lossy().into_owned(),
        url: spec.envelope.effective_url().to_owned(),
        host: host_of(spec.envelope.effective_url()),
        category: category_of(spec, filename),
        state: if spec.start_paused {
            JobState::Paused
        } else {
            JobState::Queued
        },
        priority: spec.priority,
        total: spec.envelope.content_length,
        completed: 0,
        mode: TransferMode::Single,
        protocol: None,
        addresses: 0,
        created_at: now_secs(),
        finished_at: None,
        verified: None,
        retries: Default::default(),
        media: None,
    }
}

/// The name to use before the server has been asked. `Content-Disposition` usually wins
/// later, but a job needs a row in the list the instant it is submitted.
pub fn filename_of(spec: &JobSpec) -> String {
    spec.filename
        .as_deref()
        .map(naming::sanitize)
        .or_else(|| {
            spec.envelope
                .filename_hint
                .as_deref()
                .map(naming::sanitize)
        })
        // A stream has no filename anywhere in its URL; the manifest's title is the name.
        .or_else(|| spec.media.as_ref().map(|media| naming::sanitize(&media.title)))
        .or_else(|| naming::from_url(spec.envelope.effective_url()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "download".to_owned())
}

/// Which folder the job lands in, and therefore which category the row shows.
///
/// This is decided **before** the probe, because the destination directory is an input to
/// the transfer, not an output of it. The extension supplies the browser's MIME type and
/// filename with every captured request, and the New Download sheet has already probed, so
/// the guess is informed in both real paths. Re-categorising after the probe would leave a
/// row labelled `Video` sitting in the `Documents` folder, which is worse than a rare miss.
pub fn category_of(spec: &JobSpec, filename: &str) -> Category {
    if let Some(category) = spec.category {
        return category;
    }
    // A stream is video whatever its title happens to end in.
    if spec.media.is_some() {
        return Category::Video;
    }
    naming::categorize(filename, spec.envelope.mime_type.as_deref())
}

/// The `.vxpart` the engine will pick for a job that has not run yet. It reclaims an
/// existing sidecar by exact name, so this matches for every resume; a genuinely new job
/// that collides gets a different name, and the engine tells us which one it chose.
pub fn expected_part(dest_dir: &Path, filename: &str) -> PathBuf {
    part_of(&dest_dir.join(naming::sanitize(filename)))
}

/// The partial that belongs to a destination: `<destination>.vxpart`, the same name the
/// engine and the media job both build for themselves.
pub fn part_of(dest: &Path) -> PathBuf {
    let mut path = dest.as_os_str().to_owned();
    path.push(".vxpart");
    PathBuf::from(path)
}

pub fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// How long a stalled job waits before trying again. Ten minutes is the ceiling because a
/// job that has been stalled for ten minutes is waiting on something — a router, a laptop
/// lid — that a faster retry cannot fix.
pub fn backoff(attempts: u32) -> Duration {
    const BASE: u64 = 5;
    const CEILING: u64 = 600;
    Duration::from_secs((BASE << attempts.min(7)).min(CEILING))
}

/// Queue order: priority first, then the order the user asked for them.
pub fn queue_key(view: &JobView) -> (u8, u64) {
    let priority = match view.priority {
        Priority::High => 0,
        Priority::Normal => 1,
        Priority::Low => 2,
    };
    (priority, view.id.0)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vortex_proto::RequestEnvelope;

    #[test]
    fn a_job_has_a_name_before_the_server_is_asked() {
        let spec = JobSpec::file(RequestEnvelope::new(
            "https://example.com/files/Ubuntu%2024.04.iso?token=abc",
        ));
        assert_eq!(filename_of(&spec), "Ubuntu 24.04.iso");

        let bare = JobSpec::file(RequestEnvelope::new("https://example.com/"));
        assert_eq!(filename_of(&bare), "download");
    }

    #[test]
    fn the_browsers_mime_type_decides_the_folder() {
        let mut envelope = RequestEnvelope::new("https://example.com/watch?v=1");
        envelope.mime_type = Some("video/mp4".into());
        let spec = JobSpec::file(envelope);
        assert_eq!(category_of(&spec, &filename_of(&spec)), Category::Video);
    }

    #[test]
    fn the_host_survives_ports_and_credentials() {
        assert_eq!(host_of("https://user:pw@Example.COM:8443/x"), "example.com");
        assert_eq!(host_of("https://cdn.example.com/x/y.iso"), "cdn.example.com");
    }

    #[test]
    fn backoff_climbs_and_then_stops_climbing() {
        assert_eq!(backoff(0), Duration::from_secs(5));
        assert_eq!(backoff(3), Duration::from_secs(40));
        assert_eq!(backoff(99), Duration::from_secs(600));
    }

    #[test]
    fn high_priority_jumps_the_queue_but_ties_keep_their_order() {
        let spec = JobSpec::file(RequestEnvelope::new("https://example.com/a"));
        let dir = PathBuf::from("/tmp");
        let mut first = new_view(JobId(1), &spec, &dir, "a");
        let second = new_view(JobId(2), &spec, &dir, "b");
        assert!(queue_key(&first) < queue_key(&second));
        first.priority = Priority::Low;
        assert!(queue_key(&second) < queue_key(&first));
    }
}
