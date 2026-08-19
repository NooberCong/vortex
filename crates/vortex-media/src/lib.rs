//! The Vortex media pipeline (04).
//!
//! ```text
//! manifest URL → parse → ladder → select → resolve → fetch → decrypt → mux → finalize
//! ```
//!
//! Stages 5 and 8 belong to [`vortex_engine`]; everything else is here. The shape of the
//! crate follows the shape of the pipeline: [`hls`] and [`dash`] parse, [`plan`] turns a
//! user's choice into an ordered list of segments, [`fetch`] moves them through the same
//! transport as any other download, [`crypto`] decrypts AES-128 in flight, and [`mux`]
//! hands the result to ffmpeg with `-c copy`.
//!
//! Two boundaries are load-bearing:
//!
//! * **[`drm`] refuses before anything is fetched.** A protected manifest is a product
//!   boundary, not an error, and the user is told which system is protecting it.
//! * **[`ytdlp`] extracts, it never downloads.** Sites with bespoke obfuscation get their
//!   URLs resolved by yt-dlp and then fetched by our scheduler, so resume, hedging and
//!   adaptive concurrency still apply.

#![forbid(unsafe_code)]

pub mod crypto;
pub mod dash;
pub mod drm;
pub mod fetch;
pub mod hls;
pub mod job;
pub mod mux;
pub mod net;
pub mod plan;
pub mod vtt;
pub mod ytdlp;

use vortex_engine::EngineError;

/// A URL, optionally narrowed to one byte range of it. HLS `EXT-X-BYTERANGE`, DASH
/// `SegmentBase` and `mediaRange` all describe the same thing, so they share a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub url: String,
    /// `(offset, length)`.
    pub range: Option<(u64, u64)>,
}

impl Resource {
    pub fn whole(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            range: None,
        }
    }
}

/// Why a media job could not proceed. Kept separate from [`EngineError`] because two of
/// these are not failures at all: one is a refusal and one is an honest "not yet".
#[derive(Debug)]
pub enum Error {
    /// DRM. Not retryable, not a bug, and the message says which system (04 §DRM).
    Refused(drm::Refusal),
    /// Legal, but this build cannot do it. Different sentence, deliberately.
    Unsupported(drm::Unsupported),
    /// The manifest was fetched and was not something we could read.
    Unreadable(String),
    Transfer(EngineError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused(r) => f.write_str(&r.message),
            Error::Unsupported(u) => f.write_str(&u.message),
            Error::Unreadable(m) => f.write_str(m),
            Error::Transfer(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// The sentence a person reads, already in the interface's voice (05 §Copy).
    pub fn user_message(&self) -> String {
        match self {
            Error::Refused(r) => r.message.clone(),
            Error::Unsupported(u) => u.message.clone(),
            Error::Unreadable(m) => m.clone(),
            Error::Transfer(e) => e.user_message(),
        }
    }
}

impl From<EngineError> for Error {
    fn from(e: EngineError) -> Self {
        Error::Transfer(e)
    }
}

impl From<drm::Refusal> for Error {
    fn from(r: drm::Refusal) -> Self {
        Error::Refused(r)
    }
}

impl From<drm::Unsupported> for Error {
    fn from(u: drm::Unsupported) -> Self {
        Error::Unsupported(u)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
