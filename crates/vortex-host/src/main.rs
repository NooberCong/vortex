//! `vortex-host` — the browser's end of the wire (01 §Process model).
//!
//! ```text
//! extension  ──stdio, JSON──▶  vortex-host  ──named pipe, MessagePack──▶  vortexd
//! ```
//!
//! It owns nothing and remembers nothing. The browser starts one per profile and kills it
//! whenever it likes; every byte of state is on the other side of the pipe. Two rules
//! follow from that, and they are the whole design:
//!
//! 1. **If the daemon cannot be reached, say so and leave.** The extension treats a closed
//!    port as `Disconnected` and goes fully passive — it will not cancel a browser download
//!    it cannot hand over (03 §2). A host that pretended to be connected would eat
//!    downloads.
//! 2. **Never exceed 1 MB towards the browser.** That is a hard platform limit, and
//!    tripping it kills the port silently.

// Windowless on Windows. The browser starts this one, and it starts it as a console
// program — which on a console-subsystem binary means a black window appearing over the
// page for as long as the port is open. Nothing is lost: stdin and stdout are the pipes
// the browser handed us and a subsystem does not change them, and everything this process
// says for a human's benefit goes to stderr, which the browser writes to its own log.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod native;

use tokio::io::AsyncWrite;
use vortex_proto::codec;
use vortex_proto::{Command, Event};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // stdout is the wire. Anything that is not a framed message must go to stderr, which
    // the browser writes to its own log.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let stream = match vortex_ipc::connect_or_start().await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::warn!("no daemon: {e}");
            // One honest event, then the port closes and the extension goes passive.
            let _ = tell(
                &mut stdout,
                &Event::Error {
                    message: "Vortex isn't running.".into(),
                },
            )
            .await;
            return;
        }
    };

    let (mut daemon_reader, mut daemon_writer) = tokio::io::split(stream);

    let to_daemon = async {
        while let Ok(Some(message)) = native::read(&mut stdin).await {
            let command: Command = match serde_json::from_slice(&message) {
                Ok(command) => command,
                Err(e) => {
                    // The extension and the daemon share generated types, so this is a bug
                    // rather than a user problem. Drop the message, keep the port.
                    tracing::warn!("the extension sent something unreadable: {e}");
                    continue;
                }
            };
            if codec::write_frame(&mut daemon_writer, &command).await.is_err() {
                break;
            }
        }
    };

    let to_browser = async {
        while let Ok(Some(event)) = codec::read_frame::<_, Event>(&mut daemon_reader).await {
            if tell(&mut stdout, &event).await.is_err() {
                break;
            }
        }
    };

    // Either half ending ends the relay: a dead daemon must close the port rather than
    // leave the extension believing it still has one.
    tokio::select! {
        _ = to_daemon => {}
        _ = to_browser => {}
    }
}

/// Serialises one event for the browser, replacing anything too large with a message the
/// extension can actually act on.
async fn tell<W: AsyncWrite + Unpin>(writer: &mut W, event: &Event) -> std::io::Result<()> {
    let body = serde_json::to_vec(event)?;
    if body.len() > native::MAX_TO_BROWSER {
        let replacement = serde_json::to_vec(&Event::Error {
            message: "That answer was too large to send to the browser.".into(),
        })?;
        return native::write(writer, &replacement).await;
    }
    native::write(writer, &body).await
}

// Connecting — and starting `vortexd` when the browser is the first thing to want it
// after a login — lives in `vortex_ipc::connect_or_start`, shared with the desktop app so
// the two clients cannot disagree about where the daemon is or how it is spawned.
