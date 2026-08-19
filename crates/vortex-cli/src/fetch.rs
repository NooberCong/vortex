//! `vortex fetch` — the engine, in this process, with no daemon anywhere.
//!
//! This is the harness the engine was built against, and it stays for the same reason it
//! existed: when a download misbehaves, the first question is whether the engine or the
//! daemon is responsible, and this answers it in one command.

use anyhow::Result;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use vortex_engine::job::{self, Engine, EngineEvent, Interrupted, TransferSpec};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_proto::{fmt, RequestEnvelope};

pub async fn run(url: &str, dir: &str, name: Option<String>, connections: u8) -> Result<()> {
    let engine = Arc::new(Engine::new(Transport::new(TransportConfig::default())));
    let (events, mut incoming) = mpsc::channel(64);
    let (control, control_rx) = job::control();

    // Ctrl-C stops the transfer the same way a pause does, leaving it resumable — running
    // the same command again picks up where it left off.
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nstopping \u{2014} run the same command again to resume");
            control.stop();
        }
    });

    tokio::spawn(async move {
        let mut reporter = Reporter::default();
        while let Some(event) = incoming.recv().await {
            reporter.observe(event);
        }
    });

    let outcome = job::transfer(
        engine,
        TransferSpec {
            envelope: RequestEnvelope::new(url),
            dest_dir: PathBuf::from(dir),
            filename: name,
            max_connections: connections,
            write_budget: 256 << 20,
        },
        events,
        control_rx,
    )
    .await;

    match outcome {
        Ok(done) => {
            let check = match &done.verified {
                Some(v) if v.ok => format!(" \u{b7} {} verified", v.algorithm),
                Some(v) => format!(" \u{b7} {} DID NOT MATCH", v.algorithm),
                None => String::new(),
            };
            println!(
                "\n{} in {} \u{b7} {} \u{b7} {} address(es) \u{b7} {} retries ({} recovered){check}\n{}",
                fmt::bytes(done.bytes),
                fmt::eta(done.elapsed.as_secs() as u32),
                done.protocol.map(|p| p.label()).unwrap_or("HTTP"),
                done.addresses,
                done.retries.0,
                done.retries.1,
                done.path.display()
            );
            Ok(())
        }
        Err(Interrupted::Stopped) => {
            println!("\nstopped \u{2014} the partial download is still there");
            Ok(())
        }
        Err(Interrupted::NeedsDecision(decision)) => {
            anyhow::bail!("{}", describe(&decision))
        }
        Err(Interrupted::Failed(e)) => anyhow::bail!("{}", e.user_message()),
    }
}

/// One line of progress, rewritten in place.
///
/// A media job emits both byte progress and segment counts, and two writers competing for
/// the same line produce a smear. So the reporter keeps the last of each and prints one
/// line that says whichever is the more useful thing to know right now: segments while
/// they arrive, then the muxer's own percentage.
#[derive(Default)]
pub struct Reporter {
    bytes: u64,
    bps: u64,
    total: Option<u64>,
    eta: Option<u32>,
    connections: u8,
    media: bool,
}

impl Reporter {
    pub fn observe(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Probed(probe) => {
                let size = probe
                    .total
                    .map(fmt::bytes)
                    .unwrap_or_else(|| "unknown size".to_owned());
                eprintln!(
                    "{} \u{b7} {size} \u{b7} {} \u{b7} {}",
                    probe.filename,
                    probe.protocol.label(),
                    if probe.ranges { "resumable" } else { "one stream" }
                );
            }
            EngineEvent::Destination { dest, .. } => eprintln!("saving to {}", dest.display()),
            EngineEvent::Progress(frame) => {
                self.bytes = frame.completed;
                self.bps = frame.bps;
                self.total = frame.total;
                self.eta = frame.eta_secs;
                self.connections = frame.connections;
                if !self.media {
                    self.line(&format!(
                        "{:>6}  {:>12}  {:>8}  {} connections",
                        self.percent(),
                        fmt::rate(self.bps),
                        self.eta(),
                        self.connections
                    ));
                }
            }
            EngineEvent::Media(summary) => {
                self.media = true;
                match summary.mux_percent {
                    // Muxing is its own phase. An unexplained stall at 100% is the most
                    // common "it's broken" report in every downloader ever shipped (04 \u{a7}7).
                    Some(percent) => self.line(&format!("combining tracks  {percent:>3}%")),
                    None => {
                        let missing = match summary.segments_missing {
                            0 => String::new(),
                            n => format!("  ({n} unavailable)"),
                        };
                        self.line(&format!(
                            "{:>6}  {:>12}  {}/{} segments{missing}",
                            self.percent(),
                            fmt::rate(self.bps),
                            summary.segments_done,
                            summary.segments_total
                        ));
                    }
                }
            }
            EngineEvent::UrlExpired(hint) => {
                eprintln!(
                    "\nthe link expired ({}) \u{2014} nothing here can re-mint it",
                    hint.reason
                );
            }
            EngineEvent::Phase(_) => {}
        }
    }

    fn percent(&self) -> String {
        match self.total {
            Some(total) if total > 0 => fmt::percent(self.bytes, total),
            _ => fmt::bytes(self.bytes),
        }
    }

    fn eta(&self) -> String {
        self.eta.map(fmt::eta).unwrap_or_else(|| "\u{2014}".to_owned())
    }

    /// A progress bar that scrolls is a log, not a bar. The padding covers the tail of a
    /// longer previous line.
    fn line(&self, text: &str) {
        eprint!("\r{text}                    ");
        let _ = std::io::stderr().flush();
    }
}

pub fn describe(decision: &vortex_proto::Decision) -> String {
    use vortex_proto::Decision;
    match decision {
        Decision::FileChanged { .. } => "The file changed on the server.".into(),
        Decision::DiskFull { needed, drive } => {
            format!("Not enough room on {drive}. Needs {} more.", fmt::bytes(*needed))
        }
        Decision::NameConflict { path } => format!("{path} already exists."),
        Decision::PermissionDenied { path } => format!("Vortex can't write to {path}."),
        Decision::UrlUnrecoverable { .. } => {
            "The link expired. Reopen the page to continue.".into()
        }
        Decision::IntegrityMismatch { detail } => {
            format!("The file didn't match its checksum. {detail}")
        }
        Decision::MuxFailed { detail } => format!("Combining the streams failed. {detail}"),
    }
}
