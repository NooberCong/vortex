//! Turning protocol types into lines a person can read.
//!
//! The numbers go through [`vortex_proto::fmt`], the same code the desktop app's TypeScript
//! mirrors, so `1.21 GB` here and `1.21 GB` there are the same 1.21 GB.

use vortex_proto::{fmt, JobId, JobState, JobView, Outcome, ProbeResult, SummaryFrame};

pub fn row(job: &JobView) -> String {
    let size = match job.total {
        Some(total) if total > 0 => format!(
            "{} / {}",
            fmt::bytes(job.completed),
            fmt::bytes(total)
        ),
        _ => fmt::bytes(job.completed),
    };
    format!(
        "{:>4}  {:<10}  {:>20}  {}",
        job.id.0,
        state(&job.state),
        size,
        job.filename
    )
}

pub fn summary(job: JobId, frame: &SummaryFrame) -> String {
    let percent = match frame.total {
        Some(total) if total > 0 => fmt::percent(frame.completed, total),
        _ => fmt::bytes(frame.completed),
    };
    let eta = frame
        .eta_secs
        .map(fmt::eta)
        .unwrap_or_else(|| "\u{2014}".to_owned());
    format!(
        "{:>4}  {percent:>6}  {:>12}  {eta:>8}  {} connections",
        job.0,
        fmt::rate(frame.bps),
        frame.connections
    )
}

/// The vocabulary of the interface, unchanged (05 §Copy): an action keeps its name through
/// the whole flow, so a paused job says `paused` and never `suspended`.
pub fn state(state: &JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Probing => "probing",
        JobState::Downloading => "downloading",
        JobState::Stalled { .. } => "stalled",
        JobState::Muxing => "combining",
        JobState::Paused => "paused",
        JobState::NeedsDecision { .. } => "needs you",
        JobState::Completed => "done",
        JobState::Failed { .. } => "failed",
    }
}

pub fn outcome(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Completed {
            path,
            bytes,
            elapsed_secs,
            verified,
        } => {
            let check = match verified {
                Some(v) if v.ok => format!(" \u{b7} {} verified", v.algorithm),
                Some(v) => format!(" \u{b7} {} DID NOT MATCH", v.algorithm),
                None => String::new(),
            };
            format!(
                "done  {} in {}{check}  \u{2192} {path}",
                fmt::bytes(*bytes),
                fmt::eta(*elapsed_secs)
            )
        }
        Outcome::Cancelled => "cancelled".to_owned(),
        Outcome::Failed { error } => format!("failed  {error}"),
    }
}

pub fn probe(probe: &ProbeResult) -> String {
    let size = probe
        .size
        .map(fmt::bytes)
        .unwrap_or_else(|| "unknown size".to_owned());
    let mode = match probe.mode {
        vortex_proto::TransferMode::Parallel => "parallel",
        vortex_proto::TransferMode::Single => "one connection",
    };
    let resumable = if probe.resumable {
        "resumable"
    } else {
        "not resumable"
    };
    format!(
        "{}\n  {size} \u{b7} {mode} \u{b7} {resumable} \u{b7} {} \u{b7} {}",
        probe.filename,
        probe.category.label(),
        probe.host
    )
}
