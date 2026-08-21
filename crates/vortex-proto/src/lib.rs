//! The Vortex IPC protocol.
//!
//! Every type that crosses a process boundary is defined exactly once, here, and exported
//! to TypeScript with `ts-rs` (`cargo test -p vortex-proto`) so the daemon, the desktop app
//! and the extension cannot drift.
//!
//! Wire format: 4-byte little-endian length prefix followed by a MessagePack body.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

#[cfg(feature = "codec")]
pub mod codec;

pub mod fmt;
pub mod ids;

pub use ids::{JobId, TabId};

/// Maximum accepted frame size. Guards against a hostile or confused peer.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Bump when a breaking change lands; the daemon refuses mismatched clients.
pub const PROTOCOL_VERSION: u32 = 1;

// ─────────────────────────────────────────────────────────────────────────────
// Capture
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the browser sent, captured verbatim. Replaying this exactly is what makes
/// handoff work; divergence is the number one cause of a 403 after takeover.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RequestEnvelope {
    pub url: String,
    /// Post-redirect URL, when the browser already knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(default = "default_method")]
    pub method: String,
    /// Ordered and case-preserved, exactly as the browser sent them.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Serialized cookie header value, including HttpOnly cookies the page cannot read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<TabId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    #[serde(default)]
    pub captured_at: u64,
}

fn default_method() -> String {
    "GET".to_owned()
}

impl RequestEnvelope {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: default_method(),
            ..Default::default()
        }
    }

    /// The URL workers should actually use.
    pub fn effective_url(&self) -> &str {
        self.final_url.as_deref().unwrap_or(&self.url)
    }

    /// Header lookup, case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn set_header(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self
            .headers
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
        {
            Some(slot) => slot.1 = value,
            None => self.headers.push((name.to_owned(), value)),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Jobs
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobSpec {
    pub envelope: RequestEnvelope,
    /// Absolute destination directory. `None` = the configured default for the category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_dir: Option<String>,
    /// Explicit filename, already chosen by the user. `None` = derive it (04 §8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub start_paused: bool,
    /// Present when this job assembles a stream rather than fetching one file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaSelection>,
}

impl JobSpec {
    pub fn file(envelope: RequestEnvelope) -> Self {
        Self {
            envelope,
            dest_dir: None,
            filename: None,
            category: None,
            priority: Priority::Normal,
            start_paused: false,
            media: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Category {
    Video,
    Audio,
    Archives,
    Documents,
    Images,
    Programs,
    Other,
}

impl Category {
    /// The order the app's filter bar draws them in (05 §Layout). `KINDS` in
    /// `store.svelte.ts` is this list, and has to stay this list.
    pub const ALL: [Category; 7] = [
        Category::Video,
        Category::Audio,
        Category::Archives,
        Category::Documents,
        Category::Images,
        Category::Programs,
        Category::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Video => "Video",
            Category::Audio => "Audio",
            Category::Archives => "Archives",
            Category::Documents => "Documents",
            Category::Images => "Images",
            Category::Programs => "Programs",
            Category::Other => "Other",
        }
    }
}

/// The vocabulary the UI signposts with. An action keeps its name through the whole flow
/// (05 §Copy), so these are user-visible states, not internal jargon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobState {
    Queued,
    Probing,
    Downloading,
    /// Retrying in the background with full resumable state. Nothing is lost.
    Stalled { reason: String },
    /// Segments are down; ffmpeg is combining them.
    Muxing,
    Paused,
    /// Waiting on the user. The job is intact and resumable.
    NeedsDecision { decision: Decision },
    Completed,
    /// Terminal. Only 404/410-class failures reach this.
    Failed { error: String },
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobState::Completed | JobState::Failed { .. })
    }
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            JobState::Probing | JobState::Downloading | JobState::Muxing
        )
    }
}

/// How the engine will move the bytes. Decided by the probe and shown in the New Download
/// sheet before the user commits (05 §Screens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum TransferMode {
    /// Ranges work and the size is known.
    Parallel,
    /// One connection: no range support, unknown size, or below the parallelism floor.
    Single,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum Protocol {
    Http11,
    H2,
    H3,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Http11 => "HTTP/1.1",
            Protocol::H2 => "HTTP/2",
            Protocol::H3 => "HTTP/3 · QUIC",
        }
    }
}

/// The full record of a job. Sent on `JobAdded` and on any structural change.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: JobId,
    pub filename: String,
    pub dest_path: String,
    pub url: String,
    pub host: String,
    pub category: Category,
    pub state: JobState,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub completed: u64,
    pub mode: TransferMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    /// Distinct resolved addresses in use — "4 addresses · releases.ubuntu.com".
    #[serde(default)]
    pub addresses: u8,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    /// `Some` once a server-offered checksum has been checked (02 §7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<Verification>,
    #[serde(default)]
    pub retries: Retries,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaSummary>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Retries {
    pub total: u32,
    pub recovered: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Verification {
    /// e.g. "SHA-256". Named in the UI on hover; never a fake green tick.
    pub algorithm: String,
    pub ok: bool,
}

/// 20 Hz, `Detail` subscribers only. Roughly 300 bytes thanks to the RLE runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProgressFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub completed: u64,
    /// EWMA, 3 s half-life — smoothed before it is ever displayed.
    pub bps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u32>,
    pub connections: u8,
    pub block_size: u32,
    pub blocks: u32,
    /// Run-length encoded block bitmap: `(start_block, len, owner)`.
    /// `owner` 0 = complete, 1..=N = in flight by worker N.
    pub runs: Vec<(u32, u32, u8)>,
    /// Per-worker readout for the expanded row (05 §Segment map).
    #[serde(default)]
    pub workers: Vec<WorkerFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkerFrame {
    /// 1-based lane number. The expanded view numbers lanes so color is never the sole
    /// carrier of meaning (05 §Quality floor).
    pub lane: u8,
    pub bps: u64,
    /// Recent throughput samples for the sparkline, oldest first.
    pub spark: Vec<u32>,
    pub stealing_from: bool,
}

/// 2 Hz, `Summary` subscribers. Everything a collapsed row needs and nothing more.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SummaryFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub completed: u64,
    pub bps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_secs: Option<u32>,
    pub connections: u8,
    pub blocks: u32,
    /// Coarse map for the 6 px collapsed bar.
    pub runs: Vec<(u32, u32, u8)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Outcome {
    Completed {
        path: String,
        bytes: u64,
        elapsed_secs: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verified: Option<Verification>,
    },
    Cancelled,
    Failed {
        error: String,
    },
}

/// A question only the user can answer. Every variant leaves the job fully resumable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Decision {
    /// 200 to an `If-Range` that should have given 206. Never merge (02 §5).
    FileChanged { detail: String },
    DiskFull { needed: u64, drive: String },
    NameConflict { path: String },
    PermissionDenied { path: String },
    /// Renewal failed. "Reopen the page to continue."
    UrlUnrecoverable {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_url: Option<String>,
    },
    IntegrityMismatch { detail: String },
    /// ffmpeg failed but every segment is on disk. `[ Retry mux ]`.
    MuxFailed { detail: String },
}

/// The user's answer to a `Decision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Resolution {
    StartOver,
    /// Keep the old partial as a separate file and start fresh.
    KeepBoth,
    Retry,
    Cancel,
    ChangeFolder { dest_dir: String },
    Rename { filename: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RenewalHint {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<TabId>,
    /// Why the URL is considered dead. For logs, not for the user.
    pub reason: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Probe results
// ─────────────────────────────────────────────────────────────────────────────

/// What the New Download sheet renders before the user commits (05 §Screens).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub url: String,
    pub final_url: String,
    pub filename: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub mode: TransferMode,
    pub resumable: bool,
    pub category: Category,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_dir: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Media
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum MediaKind {
    Hls,
    Dash,
    /// A plain progressive media file — no manifest.
    Progressive,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MediaCandidate {
    /// Stable id for this manifest within the tab.
    pub id: String,
    pub manifest_url: String,
    pub kind: MediaKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    pub live: bool,
    pub variants: Vec<MediaVariant>,
    pub audio: Vec<MediaTrack>,
    pub subtitles: Vec<MediaTrack>,
    /// Index into `variants` nearest the resolution actually playing (04 §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_variant: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MediaVariant {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub bandwidth: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_label: Option<String>,
    /// `bandwidth × duration / 8`. A peak declaration — the UI marks it `~` (04 §1-2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MediaTrack {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    pub default: bool,
}

/// What the overlay's Download button submits.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MediaSelection {
    pub manifest_url: String,
    pub kind: MediaKind,
    pub title: String,
    pub variant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_id: Option<String>,
    #[serde(default)]
    pub subtitle_ids: Vec<String>,
    #[serde(default)]
    pub container: ContainerPreference,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ContainerPreference {
    /// MP4 when the codecs allow it, MKV when they don't — with the reason shown.
    #[default]
    Auto,
    Mp4,
    Mkv,
}

/// Live media detail for the row: which phase, and how many segments are missing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MediaSummary {
    pub segments_total: u32,
    pub segments_done: u32,
    pub segments_missing: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mux_percent: Option<u8>,
    /// e.g. "Saved as MKV — MP4 can't hold Opus audio."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_note: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Settings
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub download_dir: String,
    /// Per-category subdirectory, relative to `download_dir` unless absolute.
    pub category_dirs: BTreeMap<String, String>,
    /// Ceiling, not a target — the controller finds the real number per origin.
    pub max_connections: u8,
    pub max_concurrent_jobs: u8,
    /// Bytes per second, 0 = unlimited.
    pub global_speed_limit: u64,
    pub enable_h3: bool,
    pub enable_capture: bool,
    /// Origins the user opted out of capture on.
    pub site_optouts: Vec<String>,
    pub min_capture_bytes: u64,
    pub default_media_height: u32,
    pub container: ContainerPreference,
    pub subtitles: bool,
    pub theme: Theme,
    pub reduced_motion: bool,
    pub clipboard_monitor: bool,
    pub autostart: bool,
    /// Memory ceiling for the writer's reorder window, in bytes.
    pub write_buffer_budget: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: String::new(),
            category_dirs: BTreeMap::new(),
            max_connections: 16,
            max_concurrent_jobs: 4,
            global_speed_limit: 0,
            enable_h3: true,
            enable_capture: true,
            site_optouts: Vec::new(),
            min_capture_bytes: 1024 * 1024,
            default_media_height: 1080,
            container: ContainerPreference::Auto,
            subtitles: true,
            theme: Theme::System,
            reduced_motion: false,
            clipboard_monitor: false,
            autostart: true,
            write_buffer_budget: 256 * 1024 * 1024,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The protocol
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SubscriptionScope {
    /// 2 Hz summaries for every job. What the list uses.
    Summary,
    /// 20 Hz detail including the per-worker map. Only an expanded row asks for this.
    Detail { job: JobId },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "cmd", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Command {
    Hello { client: String, protocol: u32 },
    Submit { spec: JobSpec },
    Pause { job: JobId },
    Resume { job: JobId },
    Cancel { job: JobId },
    Remove { job: JobId, delete_file: bool },
    Reprioritize { job: JobId, priority: Priority },
    Decide { job: JobId, resolution: Resolution },
    RetryMux { job: JobId },
    /// "What is this URL?" — drives the New Download sheet.
    Probe { envelope: RequestEnvelope },
    /// "What streams are here?" — drives the page overlay.
    ProbeMedia { envelope: RequestEnvelope },
    Subscribe { scope: SubscriptionScope },
    List,
    /// Takes the completed rows out of the list. Only the completed ones: a failed
    /// download still owns a resumable `.vxpart`, and tidying is not a decision about
    /// anyone's data.
    ClearCompleted,
    GetSettings,
    SetSettings { settings: Settings },
    /// The extension answering `UrlExpired`.
    RenewedUrl { job: JobId, envelope: RequestEnvelope },
    /// "Bring the window up, and put this job in front of me."
    ///
    /// The one command whose subject is the *app* rather than the queue, and it exists
    /// because the extension cannot reach the window: they are two processes that share a
    /// daemon and nothing else. `None` is a bare "open Vortex", which is what the popup's
    /// footer asks for.
    ///
    /// The daemon does not own a window either — it starts one, and a second copy of an
    /// app that is already running is folded into the first by the single-instance plugin
    /// (`apps/desktop/src-tauri/src/lib.rs`). So one path covers both "it is not running"
    /// and "it is behind the browser".
    Reveal { job: Option<JobId> },
    /// Capture heartbeat. The extension refuses to cancel a browser download unless this
    /// round-trips (03 §2 — the non-negotiable rule).
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "event", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    Hello { daemon: String, protocol: u32 },
    Pong,
    JobAdded { job: JobView },
    JobProgress { job: JobId, frame: ProgressFrame },
    JobSummary { job: JobId, frame: SummaryFrame },
    JobStateChanged { job: JobId, state: JobState },
    JobFinished { job: JobId, outcome: Outcome },
    JobRemoved { job: JobId },
    /// The engine needs a fresh URL. The extension re-mints it in page context.
    UrlExpired { job: JobId, hint: RenewalHint },
    MediaFound { tab: TabId, candidates: Vec<MediaCandidate> },
    Probed { result: ProbeResult },
    SettingsChanged { settings: Settings },
    Jobs { jobs: Vec<JobView> },
    /// A command could not be carried out. Carries the user-facing sentence, already in
    /// the interface's voice (05 §Copy).
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ts-rs` exports types, not constants, so the two numbers every peer has to agree on
    /// would otherwise be hand-copied into TypeScript and quietly drift. This writes them
    /// beside the generated types, by the same mechanism and on the same command.
    #[test]
    fn the_constants_are_exported_beside_the_types() {
        let dir = std::path::PathBuf::from(
            std::env::var("TS_RS_EXPORT_DIR").unwrap_or_else(|_| "bindings".into()),
        );
        std::fs::create_dir_all(&dir).expect("the bindings directory");
        let body = format!(
            "// This file was generated by `cargo test -p vortex-proto`. \
             Do not edit this file manually.\n\
             export const PROTOCOL_VERSION = {PROTOCOL_VERSION};\n\
             export const MAX_FRAME = {MAX_FRAME};\n"
        );
        std::fs::write(dir.join("constants.ts"), body).expect("writing the constants");
    }

    #[test]
    fn command_roundtrips_through_messagepack() {
        let cmd = Command::Submit {
            spec: JobSpec::file(RequestEnvelope::new("https://example.com/a.iso")),
        };
        let bytes = rmp_serde::to_vec_named(&cmd).unwrap();
        let back: Command = rmp_serde::from_slice(&bytes).unwrap();
        match back {
            Command::Submit { spec } => assert_eq!(spec.envelope.url, "https://example.com/a.iso"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn envelope_header_lookup_is_case_insensitive() {
        let mut e = RequestEnvelope::new("https://x/y");
        e.set_header("User-Agent", "Mozilla");
        assert_eq!(e.header("user-agent"), Some("Mozilla"));
        e.set_header("user-agent", "Firefox");
        assert_eq!(e.headers.len(), 1);
    }

    #[test]
    fn effective_url_prefers_the_post_redirect_url() {
        let mut e = RequestEnvelope::new("https://x/y");
        assert_eq!(e.effective_url(), "https://x/y");
        e.final_url = Some("https://cdn/x".into());
        assert_eq!(e.effective_url(), "https://cdn/x");
    }

    #[test]
    fn a_progress_frame_stays_small_on_the_wire() {
        let frame = ProgressFrame {
            total: Some(4 * 1024 * 1024 * 1024),
            completed: 2 * 1024 * 1024 * 1024,
            bps: 88_000_000,
            eta_secs: Some(41),
            connections: 12,
            block_size: 1 << 20,
            blocks: 4096,
            runs: (0..40).map(|i| (i * 100, 60, (i % 8) as u8)).collect(),
            workers: Vec::new(),
        };
        let bytes = rmp_serde::to_vec_named(&frame).unwrap();
        assert!(bytes.len() < 1000, "frame was {} bytes", bytes.len());
    }
}
