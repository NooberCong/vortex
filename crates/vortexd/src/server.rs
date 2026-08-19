//! The accept loop and one connection's worth of I/O.
//!
//! A connection is two halves that die together: a reader turning frames into commands and
//! a writer draining this client's event queue. When either ends — the client closed the
//! pipe, or the daemon dropped the client — the whole connection is dropped, so a departed
//! client never leaves a task blocked on a read that will not come.

use crate::daemon::{ClientId, Msg, CLIENT_BACKLOG};
use tokio::sync::mpsc;
use vortex_ipc::{Listener, ServerStream};
use vortex_proto::codec;
use vortex_proto::{Command, Event};

pub async fn serve(mut listener: Listener, tx: mpsc::Sender<Msg>) {
    let mut next: ClientId = 1;
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let client = next;
                next += 1;
                tokio::spawn(connection(stream, client, tx.clone()));
            }
            Err(e) => {
                // Losing one accept is survivable; losing the loop is not. Back off enough
                // that a permanent failure cannot spin a core.
                tracing::warn!("could not accept a connection: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

async fn connection(stream: ServerStream, client: ClientId, tx: mpsc::Sender<Msg>) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (events, mut queue) = mpsc::channel::<Event>(CLIENT_BACKLOG);
    if tx.send(Msg::Connected { client, events }).await.is_err() {
        return;
    }

    let outbound = async {
        while let Some(event) = queue.recv().await {
            if let Err(e) = codec::write_frame(&mut writer, &event).await {
                tracing::debug!(client, "client went away mid-write: {e}");
                break;
            }
        }
    };

    let inbound = async {
        loop {
            match codec::read_frame::<_, Command>(&mut reader).await {
                Ok(Some(command)) => {
                    let command = Box::new(command);
                    if tx.send(Msg::Command { client, command }).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!(client, "dropping a client that sent nonsense: {e}");
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = outbound => {}
        _ = inbound => {}
    }
    let _ = tx.send(Msg::Disconnected { client }).await;
}
