//! One supervised connection to `vortexd`.
//!
//! The app owns nothing (01 §Process model). Everything on screen arrives through here,
//! and the only state this module keeps is "am I connected right now" — which the window
//! needs in order to be honest about an empty list. An empty list because there are no
//! downloads and an empty list because the daemon is not running look identical, and
//! guessing between them is how a user concludes their queue was lost.
//!
//! Two of these run. The protocol scopes a subscription per connection (01 §IPC), so a
//! client that wants both the 2 Hz list and one job's 20 Hz segment map needs two: the
//! [`Role::Primary`] link carries everything, and the [`Role::Detail`] link carries
//! nothing but the progress frames of the row that is currently expanded. That is not a
//! workaround — it is what keeps an idle window costing the daemon nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};
use vortex_proto::{codec, Command, Event, SubscriptionScope};

/// Reconnect backoff. The daemon is a local process that either exists or does not, so
/// there is nothing to be gentle about — but a tight loop against a machine that is still
/// booting is a spinning fan for no reason.
const RETRY: Duration = Duration::from_millis(600);

/// User actions arrive at human speed. This exists to bound memory if something ever goes
/// wrong, not to buffer a workload.
const OUTBOX: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The list connection: subscribes to `Summary` and forwards every event.
    Primary,
    /// The expanded row: subscribes to `Detail` and forwards only its progress frames.
    Detail,
}

/// What a link does with the events it reads. The app implementation emits them to the
/// webview; tests use a channel.
pub trait Sink: Send + Sync + 'static {
    fn event(&self, event: Event);
    /// Called on every transition, and once on startup. Never called twice with the same
    /// value, so the UI can treat it as an edge.
    fn connection(&self, up: bool);
}

pub struct Link {
    role: Role,
    outbox: mpsc::Sender<Command>,
    /// Re-sent on every reconnect. A link that came back up in the wrong scope would
    /// silently stop delivering frames, which looks exactly like a frozen download.
    scope: Arc<Mutex<SubscriptionScope>>,
    up: Arc<AtomicBool>,
}

impl Link {
    pub fn spawn(role: Role, scope: SubscriptionScope, sink: Arc<dyn Sink>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(OUTBOX);
        let link = Arc::new(Self {
            role,
            outbox: tx,
            scope: Arc::new(Mutex::new(scope)),
            up: Arc::new(AtomicBool::new(false)),
        });

        let worker = link.clone();
        // `tauri::async_runtime`, not `tokio::spawn`. This runs from Tauri's `setup`, which
        // is the main thread with no reactor entered — the runtime that drives the app's
        // async work is Tauri's, and spawning onto anything else panics at startup.
        tauri::async_runtime::spawn(async move {
            worker.supervise(Arc::new(Mutex::new(rx)), sink).await;
        });
        link
    }

    /// For logs. Two connections that both say "link" are two connections nobody can tell
    /// apart at three in the morning.
    fn name(&self) -> &'static str {
        match self.role {
            Role::Primary => "primary",
            Role::Detail => "detail",
        }
    }

    pub fn connected(&self) -> bool {
        self.up.load(Ordering::Relaxed)
    }

    /// Queues a command.
    ///
    /// Fails while disconnected rather than buffering. A pause the user pressed thirty
    /// seconds ago, applied to a job that has since finished and been replaced in the list,
    /// is worse than a pause that plainly did not happen — and the window already says so.
    pub async fn send(&self, command: Command) -> Result<(), String> {
        if !self.connected() {
            return Err("Vortex isn't running.".into());
        }
        self.outbox
            .try_send(command)
            .map_err(|_| "Vortex isn't responding.".to_owned())
    }

    /// Points a `Detail` link at a different job, or at nothing.
    ///
    /// The scope is stored before it is sent so a reconnect mid-change still lands on the
    /// right job.
    pub async fn retarget(&self, scope: SubscriptionScope) -> Result<(), String> {
        *self.scope.lock().await = scope;
        if !self.connected() {
            // Not an error: the next successful connection will subscribe to this scope as
            // part of its handshake. Nothing is lost by saying nothing.
            return Ok(());
        }
        self.send(Command::Subscribe { scope }).await
    }

    async fn supervise(&self, rx: Arc<Mutex<mpsc::Receiver<Command>>>, sink: Arc<dyn Sink>) {
        loop {
            // Only the primary link may start the daemon. Two clients racing to spawn
            // `vortexd` is harmless — the second loses the endpoint bind and exits — but
            // there is no reason to have the race at all.
            let stream = match self.role {
                Role::Primary => vortex_ipc::connect_or_start().await,
                Role::Detail => vortex_ipc::connect().await,
            };
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    tracing::debug!(role = self.name(), "no daemon to connect to: {e}");
                    tokio::time::sleep(RETRY).await;
                    continue;
                }
            };

            let (mut reader, mut writer) = tokio::io::split(stream);
            if let Err(e) = self.handshake(&mut writer).await {
                tracing::warn!(role = self.name(), "handshake failed: {e}");
                tokio::time::sleep(RETRY).await;
                continue;
            }
            tracing::info!(role = self.name(), "connected");
            self.set_up(true, &sink);

            let pump = {
                let rx = rx.clone();
                let role = self.name();
                tokio::spawn(async move {
                    let mut rx = rx.lock().await;
                    while let Some(command) = rx.recv().await {
                        if let Err(e) = codec::write_frame(&mut writer, &command).await {
                            tracing::warn!(role, "command not sent: {e}");
                            break;
                        }
                    }
                    tracing::debug!(role, "outbox closed");
                })
            };

            // Read until the daemon goes away. `read_frame` is not cancel-safe — a partial
            // length prefix dropped mid-await desynchronises the stream — so it gets a task
            // to itself rather than a `select!` arm beside the outbox.
            while let Ok(Some(event)) = codec::read_frame::<_, Event>(&mut reader).await {
                if self.role == Role::Detail
                    && !matches!(event, Event::JobProgress { .. } | Event::Error { .. })
                {
                    // Structural events are broadcast to every client. Letting the second
                    // connection relay them too would deliver every `JobAdded` twice.
                    continue;
                }
                sink.event(event);
            }

            // Aborting the writer while it is parked on `recv` is clean; aborting it
            // mid-frame only ever damages a socket that is already gone.
            pump.abort();
            tracing::info!(role = self.name(), "disconnected");
            self.set_up(false, &sink);
            tokio::time::sleep(RETRY).await;
        }
    }

    async fn handshake<W>(&self, writer: &mut W) -> Result<(), codec::CodecError>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        codec::write_frame(
            writer,
            &Command::Hello {
                client: "vortex-app".into(),
                protocol: vortex_proto::PROTOCOL_VERSION,
            },
        )
        .await?;
        let scope = *self.scope.lock().await;
        codec::write_frame(writer, &Command::Subscribe { scope }).await?;
        if self.role == Role::Primary {
            // The list first, then live frames on top of it. A reconnect has to repaint
            // from scratch: jobs may have finished, failed or been added while the app was
            // not listening, and there is no delta to apply.
            codec::write_frame(writer, &Command::List).await?;
            codec::write_frame(writer, &Command::GetSettings).await?;
        }
        Ok(())
    }

    fn set_up(&self, up: bool, sink: &Arc<dyn Sink>) {
        if self.up.swap(up, Ordering::Relaxed) != up {
            sink.connection(up);
        }
    }
}
