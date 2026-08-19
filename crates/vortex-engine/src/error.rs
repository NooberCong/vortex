//! The failure taxonomy (02 §6).
//!
//! Every error is classified *before* anything is retried. This table is the reason claim
//! #1 holds: a job walks down the degradation ladder rather than failing, and the only
//! terminal state is a genuinely gone resource.

use std::fmt;
use vortex_proto::Decision;

/// What kind of failure this is, and therefore what the engine is allowed to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Connection reset, timeout, 5xx, 429, QUIC handshake failure.
    /// Exponential backoff with full jitter. The job never fails because a worker did.
    Transient,
    /// 403/401 on a URL that previously worked, expired signature.
    /// The extension re-mints the URL and the engine resumes on the existing bitmap.
    Renewable,
    /// Validator changed, checksum mismatch, size mismatch, a lying `Content-Range`.
    /// Stop immediately, never merge, ask the user.
    Integrity,
    /// 404, 410, 400 with no retry semantics. The only class that legitimately fails.
    Fatal,
    /// Disk full, permission denied, path too long. Pause, ask, stay resumable.
    Local,
}

impl Class {
    pub fn is_retryable(self) -> bool {
        matches!(self, Class::Transient)
    }
}

#[derive(Debug, thiserror::Error)]
pub struct EngineError {
    pub class: Class,
    pub context: String,
    /// The context is already the sentence a person should read, so `user_message` must
    /// not rewrite it. Set by failures that are not about a server: a DRM refusal says
    /// which system, and "The server wouldn't allow the download" would be a lie.
    pub verbatim: bool,
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.context)
    }
}

impl EngineError {
    pub fn new(class: Class, context: impl Into<String>) -> Self {
        Self {
            class,
            context: context.into(),
            verbatim: false,
            source: None,
        }
    }

    pub fn with_source(
        class: Class,
        context: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            class,
            context: context.into(),
            verbatim: false,
            source: Some(Box::new(source)),
        }
    }

    pub fn transient(context: impl Into<String>) -> Self {
        Self::new(Class::Transient, context)
    }
    pub fn renewable(context: impl Into<String>) -> Self {
        Self::new(Class::Renewable, context)
    }
    pub fn integrity(context: impl Into<String>) -> Self {
        Self::new(Class::Integrity, context)
    }
    pub fn fatal(context: impl Into<String>) -> Self {
        Self::new(Class::Fatal, context)
    }
    pub fn local(context: impl Into<String>) -> Self {
        Self::new(Class::Local, context)
    }

    /// Terminal, and already phrased for a person. Used where the reason is a product
    /// boundary rather than a server response — see `vortex_media::drm`.
    pub fn refusal(message: impl Into<String>) -> Self {
        Self {
            verbatim: true,
            ..Self::new(Class::Fatal, message)
        }
    }

    /// Classify an HTTP status. `previously_worked` distinguishes "this link died" from
    /// "you were never allowed in", which is the difference between renewing and failing.
    pub fn from_status(status: u16, previously_worked: bool) -> Self {
        let class = match status {
            401 | 403 if previously_worked => Class::Renewable,
            401 | 403 => Class::Fatal,
            404 | 410 | 400 | 405 | 501 => Class::Fatal,
            408 | 425 | 429 | 500..=599 => Class::Transient,
            416 => Class::Integrity,
            _ => Class::Transient,
        };
        Self::new(class, format!("HTTP {status}"))
    }

    /// The sentence a person reads. It says what happened, in the interface's voice, and
    /// never shows a bare status code (05 §Copy). What to *do* about it is the button the
    /// UI draws from the `Decision`, not part of this string.
    pub fn user_message(&self) -> String {
        if self.verbatim {
            return self.context.clone();
        }
        match self.class {
            Class::Fatal => match self.context.as_str() {
                "HTTP 404" | "HTTP 410" => "The file is no longer on the server.".to_owned(),
                "HTTP 401" | "HTTP 403" => "The server wouldn't allow the download.".to_owned(),
                other => format!("The server wouldn't allow the download. ({other})"),
            },
            Class::Renewable => "The link expired.".to_owned(),
            Class::Integrity => "The file changed on the server.".to_owned(),
            Class::Local => format!("Vortex couldn't write the file. {}", self.context),
            Class::Transient => LOST_CONNECTION.to_owned(),
        }
    }

    /// The `Decision` this error should surface as. `Local` failures are resolved by the
    /// job, which knows the destination path and the drive, so they are not answered here.
    pub fn decision(&self) -> Option<Decision> {
        match self.class {
            Class::Integrity => Some(Decision::IntegrityMismatch {
                detail: self.context.clone(),
            }),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for EngineError {
    fn from(e: reqwest::Error) -> Self {
        // Everything reqwest surfaces at this level — connect, read, decode, body — is a
        // transport hiccup. Status codes are classified separately and deliberately.
        let ctx = if e.is_timeout() {
            "timed out".to_owned()
        } else if e.is_connect() {
            "could not connect".to_owned()
        } else {
            e.to_string()
        };
        EngineError::with_source(Class::Transient, ctx, e)
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        // Every local IO failure is `Local`: pause, ask, stay resumable. Nothing about a
        // full disk or a denied path is worth retrying blindly.
        EngineError::with_source(Class::Local, e.to_string(), e)
    }
}

/// `ErrorKind::StorageFull` is still unstable, so match the platform codes directly.
pub fn is_disk_full(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        // Windows: ERROR_DISK_FULL / ERROR_HANDLE_DISK_FULL
        Some(112) | Some(39) if cfg!(windows) => true,
        // Unix: ENOSPC
        Some(28) if !cfg!(windows) => true,
        _ => false,
    }
}

/// The one line that carries the whole product, and it appears exactly as written
/// (05 §Copy).
pub const LOST_CONNECTION: &str = "Lost connection. Retrying — nothing downloaded so far is lost.";

pub type Result<T> = std::result::Result<T, EngineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_403_on_a_url_that_worked_is_renewable_not_fatal() {
        assert_eq!(EngineError::from_status(403, true).class, Class::Renewable);
        assert_eq!(EngineError::from_status(403, false).class, Class::Fatal);
    }

    #[test]
    fn only_gone_resources_are_fatal() {
        assert_eq!(EngineError::from_status(404, true).class, Class::Fatal);
        assert_eq!(EngineError::from_status(410, true).class, Class::Fatal);
        assert_eq!(EngineError::from_status(503, true).class, Class::Transient);
        assert_eq!(EngineError::from_status(429, true).class, Class::Transient);
    }
}
