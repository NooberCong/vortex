//! The daemon: one loop, one owner, no locks.
//!
//! Every mutation of every job goes through a single `mpsc` and is applied by one task.
//! Clients, the engine's per-job events, the tick — all of it arrives as a [`Msg`]. That
//! is not a performance trick (40 jobs at 20 Hz is 800 messages a second, which is
//! nothing); it is what makes "pause arrived while the transfer was finishing" a matter of
//! message order rather than of lock order.
//!
//! **Idle means idle.** When nothing is running and no client is connected, the loop parks
//! on a single `recv` with no timer at all (01 §Process model). The tick only exists while
//! there is something to tick for.

use crate::frames;
use crate::jobs::{self, Intent, Job};
use crate::settings::{self, SettingsFile};
use crate::store::{Store, StoredJob};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use vortex_engine::job::{self as engine_job, Completed, Engine, EngineEvent, Interrupted, TransferSpec};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_media::job::MediaSpec;
use vortex_proto::{
    Command, Decision, Event, JobId, JobSpec, JobState, MediaCandidate, Outcome, ProbeResult,
    ProgressFrame, Resolution, Settings, SubscriptionScope, TabId,
};

/// How often the daemon looks at itself while it has something to do.
const TICK: Duration = Duration::from_millis(500);
/// A client this far behind is not reading; the connection is dropped and it can reconnect
/// and ask for the list again.
pub(crate) const CLIENT_BACKLOG: usize = 256;
/// How `Reveal` names a job to `vortex-app` on its command line.
///
/// `apps/desktop/src-tauri/src/lib.rs` reads this argument and the two spellings have to
/// agree. Written twice for the same reason `--tray` is: the app does not depend on this
/// crate, and each side names the other.
const REVEAL_FLAG: &str = "--reveal";

pub type ClientId = u64;

#[derive(Default)]
pub struct Config {
    pub data_dir: PathBuf,
    pub endpoint: String,
    /// Keep the list in memory only. Tests use it; so does `vortexd --ephemeral`.
    pub ephemeral: bool,
    /// May this daemon write outside its own data directory — the login entry, and the
    /// native-messaging manifests every browser needs before capture works at all?
    ///
    /// Off by default so that constructing a `Daemon` is inert: a test, a `--ephemeral`
    /// run and a second copy on a developer's machine must not rewrite the registry of the
    /// person running them. `vortexd` the binary turns it on.
    pub os_integration: bool,
}

pub enum Msg {
    Connected {
        client: ClientId,
        events: mpsc::Sender<Event>,
    },
    Command {
        client: ClientId,
        /// Boxed because it is by far the largest thing that crosses this channel, and the
        /// channel mostly carries 20 Hz progress frames.
        command: Box<Command>,
    },
    Disconnected {
        client: ClientId,
    },
    Engine {
        job: JobId,
        event: EngineEvent,
    },
    Finished {
        job: JobId,
        outcome: engine_job::Outcome,
    },
    Probed {
        client: ClientId,
        result: Box<Result<ProbeResult, String>>,
    },
    Media {
        client: ClientId,
        tab: TabId,
        result: Box<Result<Vec<MediaCandidate>, String>>,
    },
    Shutdown(oneshot::Sender<()>),
}

struct Client {
    events: mpsc::Sender<Event>,
    scope: SubscriptionScope,
}

pub struct Daemon {
    engine: Arc<Engine>,
    store: Store,
    settings: SettingsFile,
    jobs: BTreeMap<JobId, Job>,
    clients: BTreeMap<ClientId, Client>,
    tx: mpsc::Sender<Msg>,
    /// When the next 2 Hz tick is due. Kept as an absolute instant rather than recomputed
    /// per message: a running job delivers a progress message every 50 ms, and a deadline
    /// of "half a second from the last message" is a deadline that never arrives.
    next_tick: Instant,
    stopping: Option<oneshot::Sender<()>>,
    /// See [`Config::os_integration`].
    os_integration: bool,
    /// What the login entry was last set to. `None` until the first reconcile, so the
    /// first `apply_settings` always writes and every one after it is a comparison.
    autostart: Option<bool>,
}

impl Daemon {
    pub fn new(config: &Config, tx: mpsc::Sender<Msg>) -> anyhow::Result<Self> {
        let store = if config.ephemeral {
            Store::in_memory()?
        } else {
            Store::open(&crate::paths::db_path(&config.data_dir))?
        };
        let settings = SettingsFile::load(&crate::paths::settings_path(&config.data_dir));
        let engine = build_engine(settings.get());
        engine.policy.restore(store.policy()?);

        Ok(Self {
            engine,
            store,
            settings,
            jobs: BTreeMap::new(),
            clients: BTreeMap::new(),
            tx,
            next_tick: Instant::now() + TICK,
            stopping: None,
            os_integration: config.os_integration,
            autostart: None,
        })
    }

    /// Restores the list, then runs until told to stop.
    pub async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        // Once, from the file, before anything else can change it — an install that never
        // wrote the entry and a profile carried to a new machine both land here.
        let autostart = self.settings.get().autostart;
        self.apply_autostart(autostart);
        self.rehydrate();
        self.pump();

        loop {
            let msg = match self.next_deadline() {
                Some(deadline) => tokio::select! {
                    msg = rx.recv() => msg,
                    _ = tokio::time::sleep_until(deadline.into()) => {
                        self.tick();
                        continue;
                    }
                },
                // Nothing running, nobody watching: park on the channel. No timer, no
                // wakeups, roughly 8 MB of RSS doing nothing at all.
                None => rx.recv().await,
            };
            let Some(msg) = msg else { break };
            if self.handle(msg).await {
                break;
            }
        }
        self.persist_policy();
    }

    /// Returns `true` when the loop should end.
    async fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Connected { client, events } => {
                self.clients.insert(
                    client,
                    Client {
                        events,
                        scope: SubscriptionScope::None,
                    },
                );
                // Someone is here, so this is a good moment to notice a hand-edited file.
                self.reload_settings();
            }
            Msg::Disconnected { client } => {
                self.clients.remove(&client);
            }
            Msg::Command { client, command } => self.command(client, *command).await,
            Msg::Engine { job, event } => self.engine_event(job, event),
            Msg::Finished { job, outcome } => {
                self.finished(job, outcome);
                if self.stopping.is_some() {
                    return self.finish_shutdown();
                }
            }
            Msg::Probed { client, result } => {
                let event = match *result {
                    Ok(result) => Event::Probed { result },
                    Err(message) => Event::Error { message },
                };
                self.send(client, event);
            }
            Msg::Media { client, tab, result } => {
                let event = match *result {
                    Ok(candidates) => Event::MediaFound { tab, candidates },
                    Err(message) => Event::Error { message },
                };
                self.send(client, event);
            }
            Msg::Shutdown(reply) => {
                self.stopping = Some(reply);
                self.begin_shutdown();
                return self.finish_shutdown();
            }
        }
        false
    }

    // ── Commands ────────────────────────────────────────────────────────────

    async fn command(&mut self, client: ClientId, command: Command) {
        match command {
            Command::Hello { protocol, .. } => {
                if protocol != vortex_proto::PROTOCOL_VERSION {
                    self.send(
                        client,
                        Event::Error {
                            message: format!(
                                "This copy of Vortex speaks protocol {} and the app speaks {protocol}. Update both to the same version.",
                                vortex_proto::PROTOCOL_VERSION
                            ),
                        },
                    );
                    return;
                }
                self.send(
                    client,
                    Event::Hello {
                        daemon: env!("CARGO_PKG_VERSION").to_owned(),
                        protocol: vortex_proto::PROTOCOL_VERSION,
                    },
                );
            }
            Command::Ping => self.send(client, Event::Pong),
            Command::Subscribe { scope } => {
                if let Some(entry) = self.clients.get_mut(&client) {
                    entry.scope = scope;
                }
                // A new subscriber should not have to wait up to half a second to see
                // anything, so answer with the current state immediately.
                //
                // Only for a job that is actually running. `latest` is the last frame the
                // engine produced and it outlives the run that produced it, so replaying it
                // unconditionally hands a finished or paused job a map full of live workers
                // — one of them mid-steal at 40 MB/s, minutes after the file landed. The
                // guard belongs here rather than at the write: the window asks "what is this
                // job doing", and a job with no run is doing nothing.
                if let SubscriptionScope::Detail { job } = scope {
                    if let Some(frame) = self
                        .jobs
                        .get(&job)
                        .filter(|j| j.is_running())
                        .map(|j| j.latest.clone())
                    {
                        self.send(client, Event::JobProgress { job, frame });
                    }
                }
            }
            Command::List => {
                let jobs = self.jobs.values().map(|j| j.view.clone()).collect();
                self.send(client, Event::Jobs { jobs });
            }
            Command::Submit { spec } => self.submit(spec),
            Command::Pause { job } => self.pause(job),
            Command::Resume { job } => self.resume(job),
            Command::Cancel { job } => self.cancel(job),
            Command::Remove { job, delete_file } => self.remove(job, delete_file),
            Command::Reprioritize { job, priority } => {
                if let Some(entry) = self.jobs.get_mut(&job) {
                    entry.view.priority = priority;
                    let view = entry.view.clone();
                    self.persist(job);
                    self.broadcast(Event::JobAdded { job: view });
                    self.pump();
                }
            }
            Command::Decide { job, resolution } => self.decide(job, resolution),
            Command::RetryMux { job } => {
                // The segments are still in the work directory, so retrying the mux is a
                // resume: the run skips straight past the download and back to ffmpeg.
                let combinable = matches!(
                    self.jobs.get(&job).map(|j| &j.view.state),
                    Some(JobState::NeedsDecision {
                        decision: Decision::MuxFailed { .. }
                    })
                );
                if combinable {
                    self.resume(job);
                } else {
                    self.send(
                        client,
                        Event::Error {
                            message: format!("There is nothing to combine for job {}.", job.0),
                        },
                    );
                }
            }
            Command::Probe { envelope } => {
                let engine = self.engine.clone();
                let tx = self.tx.clone();
                let suggested = self.settings.get().download_dir.clone();
                tokio::spawn(async move {
                    let result = match vortex_engine::probe::probe(&engine.transport, &envelope).await {
                        Ok(probe) => Ok(ProbeResult {
                            url: probe.url.clone(),
                            final_url: probe.final_url.clone(),
                            filename: probe.filename.clone(),
                            host: probe.host.clone(),
                            size: probe.total,
                            mime_type: probe.content_type.clone(),
                            mode: probe.mode,
                            resumable: probe.resumable(),
                            category: probe.category,
                            suggested_dir: Some(suggested),
                        }),
                        Err(e) => Err(e.user_message()),
                    };
                    let _ = tx
                        .send(Msg::Probed {
                            client,
                            result: Box::new(result),
                        })
                        .await;
                });
            }
            Command::ProbeMedia { envelope } => {
                let engine = self.engine.clone();
                let tx = self.tx.clone();
                let height = self.settings.get().default_media_height;
                tokio::spawn(async move {
                    let tab = envelope.tab_id.unwrap_or(TabId(0));
                    let result = probe_media(engine, envelope, height).await;
                    let _ = tx
                        .send(Msg::Media {
                            client,
                            tab,
                            result: Box::new(result),
                        })
                        .await;
                });
            }
            // Tidying the list is not a decision about anyone's data, so it only takes the
            // rows that have nothing left to lose. A failed download is terminal in the
            // state machine but not on disk: its `.vxpart` is whole and one Retry away
            // from finishing, and `remove` would throw that away without asking. Those
            // rows keep their ×, which is where a question like that belongs.
            Command::ClearCompleted => {
                let finished: Vec<JobId> = self
                    .jobs
                    .iter()
                    .filter(|(_, job)| job.view.state == JobState::Completed)
                    .map(|(id, _)| *id)
                    .collect();
                for id in finished {
                    self.remove(id, false);
                }
            }
            Command::GetSettings => {
                let settings = self.settings.get().clone();
                self.send(client, Event::SettingsChanged { settings });
            }
            Command::SetSettings { settings } => {
                let applied = self.settings.set(settings).clone();
                self.apply_settings(&applied);
                self.broadcast(Event::SettingsChanged { settings: applied });
            }
            Command::RenewedUrl { job, envelope } => self.renewed(job, envelope),
            Command::Reveal { job } => self.reveal(client, job),
        }
    }

    fn submit(&mut self, spec: JobSpec) {
        let id = match self.store.next_job_id() {
            Ok(id) => id,
            Err(e) => {
                self.broadcast(Event::Error {
                    message: format!("Vortex couldn't record the download: {e}"),
                });
                return;
            }
        };
        let filename = jobs::filename_of(&spec);
        let category = jobs::category_of(&spec, &filename);
        let dest_dir = match spec.dest_dir.as_deref() {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => settings::dest_dir(self.settings.get(), category),
        };
        let view = jobs::new_view(id, &spec, &dest_dir, &filename);
        let part = jobs::expected_part(&dest_dir, &filename);

        self.jobs.insert(
            id,
            Job {
                view: view.clone(),
                spec,
                dest_dir,
                part,
                run: None,
                intent: Intent::Run,
                attempts: 0,
                retry_at: None,
                latest: ProgressFrame::default(),
            },
        );
        self.persist(id);
        self.broadcast(Event::JobAdded { job: view });
        self.pump();
    }

    /// Brings the window up, on behalf of a client that has no way to do it itself.
    ///
    /// The extension is the caller that matters: it lives in a browser, the window lives
    /// in `vortex-app`, and the only thing the two share is this daemon. Starting the app
    /// covers both cases at once — not running, and running behind the browser — because
    /// a second copy is folded into the first by the app's single-instance plugin.
    ///
    /// A job id is passed through as text and not looked up here. The window is the thing
    /// that knows what "show me that one" means (which filter has to move, whether the row
    /// opens), and a job the daemon has forgotten is a question for the list to answer
    /// rather than a reason to refuse to open.
    fn reveal(&mut self, client: ClientId, job: Option<JobId>) {
        let id = job.map(|job| job.0.to_string());
        let args = match &id {
            Some(id) => vec![REVEAL_FLAG, id.as_str()],
            None => Vec::new(),
        };
        if let Err(e) = vortex_ipc::open_app(&args) {
            tracing::warn!("could not open the Vortex window: {e}");
            self.send(
                client,
                Event::Error {
                    message: "Vortex couldn't open its window. The app may not be installed."
                        .to_owned(),
                },
            );
        }
    }

    fn pause(&mut self, id: JobId) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        if job.view.state.is_terminal() || job.view.state == JobState::Paused {
            return;
        }
        job.stop(Intent::Pause);
        // A queued job has nothing to wind down, so it is paused right here; a running one
        // is paused when its run lands.
        let running = job.is_running();
        if !running {
            self.set_state(id, JobState::Paused);
        }
    }

    fn resume(&mut self, id: JobId) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        if job.is_running() || job.view.state.is_terminal() {
            return;
        }
        job.intent = Intent::Run;
        job.attempts = 0;
        job.retry_at = None;
        self.set_state(id, JobState::Queued);
        self.pump();
    }

    /// Cancel discards the transfer: the partial data goes, and the row goes with it. The
    /// user is told what happened through `JobFinished{Cancelled}` before the row leaves,
    /// which is the difference between cancel and remove.
    fn cancel(&mut self, id: JobId) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.stop(Intent::Cancel);
        let running = job.is_running();
        if !running {
            self.retire(id, Intent::Cancel);
        }
    }

    fn remove(&mut self, id: JobId, delete_file: bool) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.stop(Intent::Remove { delete_file });
        let running = job.is_running();
        if !running {
            self.retire(id, Intent::Remove { delete_file });
        }
    }

    fn decide(&mut self, id: JobId, resolution: Resolution) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        if job.is_running() {
            // A decision about a run that has already restarted is stale; ignore it rather
            // than fight the running transfer.
            return;
        }
        match resolution {
            Resolution::Cancel => return self.cancel(id),
            Resolution::Retry => {}
            Resolution::StartOver => jobs::discard_part(&job.part),
            Resolution::KeepBoth => {
                // Leave the old partial exactly where it is and take the next free name;
                // the engine reclaims a `.vxpart` only when its name matches.
                let unique = vortex_engine::naming::unique_path(&job.dest_dir, &job.view.filename);
                if let Some(name) = unique.file_name().and_then(|n| n.to_str()) {
                    job.view.filename = name.to_owned();
                    job.spec.filename = Some(name.to_owned());
                    job.part = jobs::expected_part(&job.dest_dir, name);
                    job.view.dest_path = unique.to_string_lossy().into_owned();
                }
            }
            Resolution::ChangeFolder { dest_dir } => {
                let dir = PathBuf::from(dest_dir);
                job.part = jobs::expected_part(&dir, &job.view.filename);
                job.view.dest_path = dir.join(&job.view.filename).to_string_lossy().into_owned();
                job.spec.dest_dir = Some(dir.to_string_lossy().into_owned());
                job.dest_dir = dir;
            }
            Resolution::Rename { filename } => {
                let name = vortex_engine::naming::sanitize(&filename);
                job.part = jobs::expected_part(&job.dest_dir, &name);
                job.view.dest_path = job.dest_dir.join(&name).to_string_lossy().into_owned();
                job.spec.filename = Some(name.clone());
                job.view.filename = name;
            }
        }
        job.attempts = 0;
        job.intent = Intent::Run;
        let view = job.view.clone();
        self.broadcast(Event::JobAdded { job: view });
        self.set_state(id, JobState::Queued);
        self.pump();
    }

    fn renewed(&mut self, id: JobId, envelope: vortex_proto::RequestEnvelope) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.spec.envelope = envelope.clone();
        job.view.url = envelope.effective_url().to_owned();
        job.attempts = 0;
        let control = job.run.clone();
        match control {
            // The transfer is waiting on exactly this: it resumes on the bitmap it already
            // has, without re-fetching a byte (02 §6).
            Some(control) => {
                tokio::spawn(async move { control.renew(envelope).await });
            }
            None => {
                self.set_state(id, JobState::Queued);
                self.pump();
            }
        }
    }

    // ── Running ─────────────────────────────────────────────────────────────

    /// Starts as many queued jobs as the concurrency setting allows, highest priority
    /// first.
    fn pump(&mut self) {
        if self.stopping.is_some() {
            return;
        }
        let limit = self.settings.get().max_concurrent_jobs as usize;
        loop {
            let active = self.jobs.values().filter(|j| j.is_running()).count();
            if active >= limit {
                return;
            }
            let next = {
                // Two jobs that expect the same `.vxpart` cannot run at once — they would
                // write one file, and one block map, over each other. The second waits its
                // turn, by which time the first has either renamed its partial away or
                // left one behind with a name of its own.
                let busy: Vec<&Path> = self
                    .jobs
                    .values()
                    .filter(|j| j.is_running())
                    .map(|j| j.part.as_path())
                    .collect();
                self.jobs
                    .values()
                    .filter(|j| j.view.state == JobState::Queued && !j.is_running())
                    .filter(|j| !busy.contains(&j.part.as_path()))
                    .min_by_key(|j| jobs::queue_key(&j.view))
                    .map(|j| j.view.id)
            };
            match next {
                Some(id) => self.start(id),
                None => return,
            }
        }
    }

    fn start(&mut self, id: JobId) {
        let settings = self.settings.get().clone();
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        let Some(job) = self.jobs.get_mut(&id) else { return };

        let spec = TransferSpec {
            envelope: job.spec.envelope.clone(),
            dest_dir: job.dest_dir.clone(),
            filename: job.spec.filename.clone(),
            max_connections: settings.max_connections,
            write_budget: settings.write_buffer_budget,
        };
        let media = job.spec.media.clone().map(|selection| MediaSpec {
            envelope: job.spec.envelope.clone(),
            selection,
            dest_dir: job.dest_dir.clone(),
            filename: job.spec.filename.clone(),
            max_connections: settings.max_connections,
            container: settings.container,
        });
        let (control, control_rx) = engine_job::control();
        tokio::spawn(async move {
            let (events, mut rx) = mpsc::channel(64);
            // Both halves have the same signature and the same outcome, so everything
            // downstream — pause, resume, retry, the event fan-out — is shared.
            let transfer: std::pin::Pin<Box<dyn std::future::Future<Output = engine_job::Outcome> + Send>> =
                match media {
                    Some(media) => Box::pin(vortex_media::job::transfer(engine, media, events, control_rx)),
                    None => Box::pin(engine_job::transfer(engine, spec, events, control_rx)),
                };
            tokio::pin!(transfer);
            let outcome = loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        let _ = tx.send(Msg::Engine { job: id, event }).await;
                    }
                    outcome = &mut transfer => break outcome,
                }
            };
            // Whatever is still queued describes the state this run ended in; deliver it
            // before the outcome so the last frame the UI sees is the true one.
            while let Ok(event) = rx.try_recv() {
                let _ = tx.send(Msg::Engine { job: id, event }).await;
            }
            let _ = tx.send(Msg::Finished { job: id, outcome }).await;
        });

        job.run = Some(control);
        job.intent = Intent::Run;
        self.set_state(id, JobState::Probing);
    }

    fn engine_event(&mut self, id: JobId, event: EngineEvent) {
        match event {
            EngineEvent::Probed(probe) => {
                {
                    let Some(job) = self.jobs.get_mut(&id) else { return };
                    if job.spec.filename.is_none() {
                        job.view.filename = probe.filename.clone();
                    }
                    job.view.total = probe.total;
                    job.view.mode = probe.mode;
                    job.view.protocol = Some(probe.protocol);
                    job.view.addresses = probe.addresses.len().min(u8::MAX as usize) as u8;
                    job.view.host = probe.host.clone();
                    job.view.url = probe.final_url.clone();
                }
                self.announce(id);
            }
            EngineEvent::Destination { dest, part } => {
                {
                    let Some(job) = self.jobs.get_mut(&id) else { return };
                    job.part = part;
                    if let Some(name) = dest.file_name().and_then(|n| n.to_str()) {
                        job.view.filename = name.to_owned();
                    }
                    job.view.dest_path = dest.to_string_lossy().into_owned();
                }
                self.announce(id);
            }
            EngineEvent::Phase(state) => self.set_state(id, state),
            EngineEvent::Progress(frame) => {
                {
                    let Some(job) = self.jobs.get_mut(&id) else { return };
                    job.view.completed = frame.completed;
                    if frame.total.is_some() {
                        job.view.total = frame.total;
                    }
                    // Progress is proof the network came back.
                    job.attempts = 0;
                    job.latest = frame.clone();
                }
                self.fanout(id, frame);
            }
            EngineEvent::UrlExpired(hint) => self.broadcast(Event::UrlExpired { job: id, hint }),
            EngineEvent::Media(summary) => {
                {
                    let Some(job) = self.jobs.get_mut(&id) else { return };
                    job.view.media = Some(summary);
                }
                self.announce(id);
            }
        }
    }

    /// A run ended. What that means depends entirely on why it was stopped.
    fn finished(&mut self, id: JobId, outcome: engine_job::Outcome) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.run = None;
        let intent = job.intent;

        match intent {
            Intent::Cancel => return self.retire(id, intent),
            Intent::Remove { .. } => return self.retire(id, intent),
            Intent::Pause | Intent::Run => {}
        }

        match outcome {
            Ok(done) => self.completed(id, done),
            Err(Interrupted::Stopped) if intent == Intent::Pause => {
                // Logging out is not the same as pressing pause. A job the daemon stopped
                // goes back in the queue, so the next start resumes it without the user
                // doing anything — which is the whole of claim #1.
                let state = if self.stopping.is_some() {
                    JobState::Queued
                } else {
                    JobState::Paused
                };
                self.set_state(id, state);
                self.pump();
            }
            Err(Interrupted::Stopped) => {
                // Nobody asked it to stop, so the network did. Everything is on disk;
                // wait a little and go again. This is `Stalled`, not `Failed` — the job is
                // intact and the user is told so rather than asked to do anything.
                let Some(job) = self.jobs.get_mut(&id) else { return };
                let wait = jobs::backoff(job.attempts);
                job.attempts += 1;
                job.retry_at = Some(Instant::now() + wait);
                self.set_state(
                    id,
                    JobState::Stalled {
                        reason: vortex_engine::error::LOST_CONNECTION.to_owned(),
                    },
                );
                self.pump();
            }
            Err(Interrupted::NeedsDecision(decision)) => {
                self.set_state(id, JobState::NeedsDecision { decision });
                self.pump();
            }
            Err(Interrupted::Failed(error)) => {
                let message = error.user_message();
                self.set_state(id, JobState::Failed {
                    error: message.clone(),
                });
                if let Some(job) = self.jobs.get_mut(&id) {
                    job.view.finished_at = Some(jobs::now_secs());
                }
                self.persist(id);
                self.broadcast(Event::JobFinished {
                    job: id,
                    outcome: Outcome::Failed { error: message },
                });
                self.pump();
            }
        }
    }

    fn completed(&mut self, id: JobId, done: Completed) {
        let verified = done.verified.clone();
        let Some(job) = self.jobs.get_mut(&id) else { return };
        job.view.dest_path = done.path.to_string_lossy().into_owned();
        if let Some(name) = done.path.file_name().and_then(|n| n.to_str()) {
            job.view.filename = name.to_owned();
        }
        job.view.completed = done.bytes;
        job.view.total = Some(done.bytes);
        job.view.verified = verified.clone();
        job.view.finished_at = Some(jobs::now_secs());
        job.view.retries = vortex_proto::Retries {
            total: done.retries.0,
            recovered: done.retries.1,
        };
        job.view.addresses = done.addresses;
        if let Some(protocol) = done.protocol {
            job.view.protocol = Some(protocol);
        }
        job.attempts = 0;
        let path = job.view.dest_path.clone();
        let view = job.view.clone();

        self.broadcast(Event::JobAdded { job: view });
        self.set_state(id, JobState::Completed);
        self.broadcast(Event::JobFinished {
            job: id,
            outcome: Outcome::Completed {
                path,
                bytes: done.bytes,
                elapsed_secs: done.elapsed.as_secs() as u32,
                verified,
            },
        });
        self.persist_policy();
        self.pump();
    }

    /// Takes a job out of the list, with whatever cleanup its exit deserves.
    fn retire(&mut self, id: JobId, intent: Intent) {
        let Some(job) = self.jobs.remove(&id) else { return };
        // Two rows can name the same `.vxpart`: the path a job expects before it has run
        // is a guess from the filename, and asking for the same link twice makes the same
        // guess twice. The row that leaves must not take the other one's bytes with it.
        if !self.jobs.values().any(|other| other.part == job.part) {
            jobs::discard_part(&job.part);
        }
        if let Intent::Remove { delete_file: true } = intent {
            let _ = std::fs::remove_file(&job.view.dest_path);
        }
        let _ = self.store.delete_job(id);
        if intent == Intent::Cancel {
            self.broadcast(Event::JobFinished {
                job: id,
                outcome: Outcome::Cancelled,
            });
        }
        self.broadcast(Event::JobRemoved { job: id });
        self.pump();
    }

    // ── The tick ────────────────────────────────────────────────────────────

    fn next_deadline(&self) -> Option<Instant> {
        if self.jobs.values().any(Job::is_running) || !self.clients.is_empty() {
            return Some(self.next_tick);
        }
        // Nothing to report and nobody to report it to: the only thing left that could
        // want the daemon awake is a stalled job waiting to try again.
        self.jobs.values().filter_map(|job| job.retry_at).min()
    }

    fn tick(&mut self) {
        self.next_tick = Instant::now() + TICK;
        self.reload_settings();

        let now = Instant::now();
        let due: Vec<JobId> = self
            .jobs
            .values()
            .filter(|job| job.retry_at.is_some_and(|at| at <= now))
            .map(|job| job.view.id)
            .collect();
        for id in due {
            if let Some(job) = self.jobs.get_mut(&id) {
                job.retry_at = None;
            }
            self.set_state(id, JobState::Queued);
        }
        self.pump();

        if !self
            .clients
            .values()
            .any(|c| c.scope == SubscriptionScope::Summary)
        {
            return;
        }
        let summaries: Vec<(JobId, vortex_proto::SummaryFrame)> = self
            .jobs
            .values()
            .filter(|job| job.is_running())
            .map(|job| (job.view.id, frames::summary(&job.latest)))
            .collect();
        for (job, frame) in summaries {
            self.send_to_scope(SubscriptionScope::Summary, Event::JobSummary { job, frame });
        }
    }

    fn reload_settings(&mut self) {
        if let Some(settings) = self.settings.reload_if_changed() {
            let settings = settings.clone();
            self.apply_settings(&settings);
            self.broadcast(Event::SettingsChanged { settings });
        }
    }

    fn apply_settings(&mut self, settings: &Settings) {
        self.engine.limiter.set_limit(settings.global_speed_limit);
        self.apply_autostart(settings.autostart);
        // The transport is built with its ALPN policy fixed, so an h3 change makes a new
        // one. Transfers already running keep the engine they started with — restarting a
        // live download to change a protocol preference would be a worse trade.
        if self.engine.transport.config().enable_h3 != settings.enable_h3 {
            let policy = self.engine.policy.snapshot();
            self.engine = build_engine(settings);
            self.engine.policy.restore(policy);
        }
        self.pump();
    }

    /// The login entry follows the setting, and only ever in that direction. The setting
    /// is what the user edits; the entry is what they never see. Reconciling the other way
    /// would mean a Vortex uninstalled and reinstalled elsewhere silently turns itself off.
    ///
    /// Nothing is started or stopped here. Turning autostart on does not launch a second
    /// daemon, and turning it off does not kill the one running this code.
    fn apply_autostart(&mut self, enabled: bool) {
        if !self.os_integration {
            return;
        }
        // What we last asked for is not the same question as what the machine has. An
        // entry removed behind our back — an update's uninstall step, a startup-item
        // cleaner, a user with `regedit` open — would otherwise stay removed until the
        // daemon next started, and a daemon that does not start is the one case where
        // that never happens. So the fact is read too, and only agreement is a skip.
        //
        // This is a registry read on a path that runs when a client connects or the
        // settings change, not on a timer: an idle daemon still touches nothing (01
        // §Process model).
        if self.autostart == Some(enabled) && vortex_setup::autostart_enabled() == enabled {
            return;
        }
        self.autostart = Some(enabled);
        match vortex_setup::set_autostart(enabled) {
            Ok(()) => tracing::info!(enabled, "login entry"),
            // Worth a line and nothing more. A user whose registry hive is read-only has a
            // larger problem than this setting, and refusing to run is not a help.
            Err(e) => tracing::warn!("could not change the login entry: {e}"),
        }
    }

    // ── Talking to clients ──────────────────────────────────────────────────

    fn send(&mut self, client: ClientId, event: Event) {
        if let Some(entry) = self.clients.get(&client) {
            if entry.events.try_send(event).is_err() {
                self.clients.remove(&client);
            }
        }
    }

    fn broadcast(&mut self, event: Event) {
        let dead: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, client)| client.events.try_send(event.clone()).is_err())
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.clients.remove(&id);
        }
    }

    fn send_to_scope(&mut self, scope: SubscriptionScope, event: Event) {
        let mut gone = Vec::new();
        for (id, client) in &self.clients {
            if client.scope != scope {
                continue;
            }
            match client.events.try_send(event.clone()) {
                Ok(()) => {}
                // A dropped frame is a frame; the next one is 50 ms behind it. Only a
                // closed channel means the client is actually gone.
                Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => gone.push(*id),
            }
        }
        for id in gone {
            self.clients.remove(&id);
        }
    }

    fn fanout(&mut self, id: JobId, frame: ProgressFrame) {
        self.send_to_scope(
            SubscriptionScope::Detail { job: id },
            Event::JobProgress { job: id, frame },
        );
    }

    /// Re-sends the whole record. `JobAdded` is an upsert keyed by id — the `JobView`
    /// doc calls it "sent on `JobAdded` and on any structural change", and this is that.
    fn announce(&mut self, id: JobId) {
        self.persist(id);
        if let Some(view) = self.jobs.get(&id).map(|job| job.view.clone()) {
            self.broadcast(Event::JobAdded { job: view });
        }
    }

    fn set_state(&mut self, id: JobId, state: JobState) {
        let Some(job) = self.jobs.get_mut(&id) else { return };
        if job.view.state == state {
            return;
        }
        job.view.state = state.clone();
        self.persist(id);
        self.broadcast(Event::JobStateChanged { job: id, state });
    }

    // ── Durability ──────────────────────────────────────────────────────────

    fn persist(&mut self, id: JobId) {
        let Some(job) = self.jobs.get(&id) else { return };
        let stored = StoredJob {
            view: job.view.clone(),
            spec: job.spec.clone(),
            dest_dir: job.dest_dir.to_string_lossy().into_owned(),
        };
        if let Err(e) = self.store.save_job(&stored) {
            tracing::warn!(job = id.0, "could not write the job record: {e}");
        }
    }

    fn persist_policy(&mut self) {
        let snapshot = self.engine.policy.snapshot();
        if let Err(e) = self.store.save_policy(&snapshot) {
            tracing::warn!("could not write the origin policy cache: {e}");
        }
    }

    // ── Starting and stopping ───────────────────────────────────────────────

    /// Rebuilds the list: what the database remembers, plus any `.vxpart.meta` under the
    /// download roots that it does not (01 §Failure isolation). The sidecar is the
    /// authority — a lost database costs history, never bytes.
    fn rehydrate(&mut self) {
        let stored = match self.store.jobs() {
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::error!("could not read the job list: {e}");
                Vec::new()
            }
        };
        for job in stored {
            let id = job.view.id;
            let dest_dir = PathBuf::from(&job.dest_dir);
            let mut view = job.view;
            // Anything that was mid-flight when the daemon stopped goes back in the queue.
            // In-flight blocks are re-fetched, never trusted.
            if view.state.is_active() || matches!(view.state, JobState::Stalled { .. }) {
                view.state = JobState::Queued;
            }
            let part = jobs::expected_part(&dest_dir, &view.filename);
            if !view.state.is_terminal() {
                if let Some(completed) = crate::rehydrate::durable_progress(&part) {
                    view.completed = completed;
                }
            }
            self.jobs.insert(
                id,
                Job {
                    part,
                    spec: job.spec,
                    dest_dir,
                    view,
                    run: None,
                    intent: Intent::Run,
                    attempts: 0,
                    retry_at: None,
                    latest: ProgressFrame::default(),
                },
            );
        }

        let roots = crate::rehydrate::roots(self.settings.get());
        // Both names a restored job could be using. `part` is rebuilt from the filename,
        // which is a guess for anything the engine renamed — a stream muxed into a
        // different container, most of all — and the destination is the answer it gave.
        // A job listed under one name and holding a partial under the other is a job
        // whose bytes the sweep below would take for an orphan.
        let known: Vec<PathBuf> = self
            .jobs
            .values()
            .flat_map(|job| {
                [
                    job.part.clone(),
                    jobs::part_of(Path::new(&job.view.dest_path)),
                ]
            })
            .collect();
        let scan = crate::rehydrate::scan(&roots, &known);
        for found in scan.found {
            let Ok(id) = self.store.next_job_id() else { continue };
            let job = found.into_job(id);
            self.jobs.insert(id, job);
            self.persist(id);
        }
        // Nothing will ever offer these to the user, so nothing but this will ever free
        // them. Startup is the moment to do it: the tree is already being walked and not
        // one job is running, so there is nothing to race.
        for orphan in scan.orphans {
            tracing::info!("discarding orphaned partial {}", orphan.display());
            jobs::discard_part(&orphan);
        }

        let count = self.jobs.len();
        if count > 0 {
            tracing::info!("restored {count} job(s)");
        }
    }

    fn begin_shutdown(&mut self) {
        let running: Vec<JobId> = self
            .jobs
            .values()
            .filter(|job| job.is_running())
            .map(|job| job.view.id)
            .collect();
        for id in running {
            if let Some(job) = self.jobs.get_mut(&id) {
                // Shutdown is a pause. Everything durable is already written; the next
                // start resumes from the bitmap.
                job.stop(Intent::Pause);
            }
        }
        self.persist_policy();
    }

    /// Returns `true` once nothing is running and the reply has been sent.
    fn finish_shutdown(&mut self) -> bool {
        if self.jobs.values().any(Job::is_running) {
            return false;
        }
        if let Some(reply) = self.stopping.take() {
            let _ = reply.send(());
        }
        true
    }
}

/// "What streams are here?" — our own parsers first, yt-dlp only when they come up empty.
///
/// A DRM refusal is final and never falls through to the extractor: yt-dlp cannot download
/// protected video either, and pretending otherwise would turn a clear product boundary
/// into a confusing second failure (04 §DRM).
///
/// **Every path out of here logs**, and that is not decoration. A probe that finds nothing
/// emits no event, raises no badge and produces no error anyone ever sees: the extension
/// asks, the daemon answers with silence, and from outside the process that is
/// indistinguishable from never having been asked. All five capture channels end here, so
/// without these lines "no download button on that page" and "no probe ever arrived" read
/// identically in the log — which is to say the log could not be used to tell them apart,
/// which is the whole reason to keep one.
async fn probe_media(
    engine: Arc<Engine>,
    envelope: vortex_proto::RequestEnvelope,
    height: u32,
) -> Result<Vec<MediaCandidate>, String> {
    let url = loggable(envelope.effective_url()).to_owned();
    let tab = envelope.tab_id.map(|id| id.0);
    tracing::info!(url = %url, tab, "probe: asking our own parsers");

    let declined = match vortex_media::plan::inspect(&engine, &envelope, height).await {
        Ok(candidate) => {
            tracing::info!(
                url = %url,
                variants = candidate.variants.len(),
                audio = candidate.audio.len(),
                subtitles = candidate.subtitles.len(),
                live = candidate.live,
                "probe: answered by our parsers",
            );
            return Ok(vec![candidate]);
        }
        Err(vortex_media::Error::Refused(refusal)) => {
            // Final by design, and worth saying plainly: this is the product boundary
            // doing its job, not a failure to investigate (04 §DRM).
            tracing::info!(url = %url, "probe: refused as protected — {}", refusal.message);
            return Err(refusal.message);
        }
        Err(e) => e,
    };
    tracing::debug!(url = %url, "probe: our parsers declined — {declined}");

    let Some(tool) = vortex_media::ytdlp::YtDlp::find() else {
        // Not a fact about this page. Without the extractor, channel 5 cannot answer on
        // *any* page, which is worth a warning rather than another quiet nothing.
        tracing::warn!(url = %url, "probe: no yt-dlp on this machine — channel 5 cannot answer");
        return Err(declined.user_message());
    };

    match vortex_media::ytdlp::extract(&tool, &envelope, height).await {
        Ok(found) => match extracted(&engine, &envelope, found, height).await {
            Ok(candidates) => {
                tracing::info!(url = %url, count = candidates.len(), "probe: answered by the extractor");
                Ok(candidates)
            }
            // The extractor named a stream and our own parsers then could not read it.
            // Two tools disagreeing about one page is the interesting case, not a footnote.
            Err(unreadable) => {
                tracing::warn!(url = %url, "probe: the extractor found a stream we could not read — {unreadable}");
                // The extractor's own complaint is about the page; ours is about the manifest
                // we actually fetched, and it is the more useful sentence.
                Err(declined.user_message())
            }
        },
        Err(nothing) => {
            tracing::info!(url = %url, "probe: no stream on this page — {nothing}");
            Err(declined.user_message())
        }
    }
}

/// A URL as it should appear in a log: everything up to the query.
///
/// The query string is where a signed URL keeps its credentials, and a rotating file on
/// disk is the wrong place to leave one — the daemon holds a token so it can fetch a
/// manifest, not so it can write it down. Origin and path are what someone reading the log
/// is actually asking ("which site, which manifest"), and they carry no secret.
fn loggable(url: &str) -> &str {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    &url[..end]
}

/// Turns what the extractor found into candidates.
///
/// A ladder of finished URLs is already the answer. A manifest is not: it goes back
/// through our own parsers, which build the real ladder — every rung, the audio
/// renditions, the subtitle tracks — rather than the flattened list of formats an
/// extractor prints. yt-dlp's contribution there is finding *where* the stream is
/// described, which is the part our sniffer missed.
async fn extracted(
    engine: &Arc<Engine>,
    envelope: &vortex_proto::RequestEnvelope,
    found: vortex_media::ytdlp::Extracted,
    height: u32,
) -> Result<Vec<MediaCandidate>, String> {
    match found {
        vortex_media::ytdlp::Extracted::Ladder(candidate) => Ok(vec![*candidate]),
        vortex_media::ytdlp::Extracted::Manifest(url) => {
            tracing::debug!(
                manifest = %loggable(&url),
                "probe: the extractor found where the stream is described",
            );
            let mut envelope = envelope.clone();
            envelope.url = url;
            envelope.final_url = None;
            vortex_media::plan::inspect(engine, &envelope, height)
                .await
                .map(|candidate| vec![candidate])
                .map_err(|e| e.user_message())
        }
    }
}

fn build_engine(settings: &Settings) -> Arc<Engine> {
    let engine = Engine::new(Transport::new(TransportConfig {
        enable_h3: settings.enable_h3,
        ..Default::default()
    }));
    engine.limiter.set_limit(settings.global_speed_limit);
    Arc::new(engine)
}

#[cfg(test)]
mod tests {
    use super::loggable;

    /// A log line is a file on disk, and a signed URL's query is a credential.
    ///
    /// This is a privacy property rather than a formatting preference, which is why it is
    /// asserted rather than left to whoever next edits the `tracing` calls.
    #[test]
    fn a_logged_url_keeps_its_path_and_drops_its_credentials() {
        assert_eq!(
            loggable("https://cdn.example.com/hls/master.m3u8?token=abc123&Expires=99"),
            "https://cdn.example.com/hls/master.m3u8",
        );
        assert_eq!(
            loggable("https://example.com/watch/1#t=90"),
            "https://example.com/watch/1",
        );
        // Nothing to trim is the ordinary case, and it must not lose the last character.
        assert_eq!(
            loggable("https://example.com/hls/master.m3u8"),
            "https://example.com/hls/master.m3u8",
        );
        assert_eq!(loggable(""), "");
    }
}
