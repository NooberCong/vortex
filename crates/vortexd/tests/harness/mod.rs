//! A daemon on a private endpoint, and a client that talks to it exactly the way the
//! desktop app and the extension do — real frames, real pipe, no shortcuts.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::oneshot;
use vortex_ipc::ClientStream;
use vortex_proto::codec;
use vortex_proto::{Command, Event, JobId, Settings};

/// Long enough that a loaded CI machine does not fail a correct daemon, short enough that
/// a hang is a test failure rather than a coffee break.
pub const PATIENCE: Duration = Duration::from_secs(30);

static NEXT: AtomicU64 = AtomicU64::new(1);

pub fn unique_endpoint() -> String {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    if cfg!(windows) {
        format!(r"\\.\pipe\vortex-test.{pid}.{n}")
    } else {
        std::env::temp_dir()
            .join(format!("vortex-test.{pid}.{n}.sock"))
            .to_string_lossy()
            .into_owned()
    }
}

pub struct Daemon {
    pub endpoint: String,
    pub data_dir: PathBuf,
    pub downloads: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Daemon {
    /// Starts a daemon whose data directory persists, so a restart can be tested.
    pub async fn start(data_dir: &Path, downloads: &Path) -> Self {
        Self::start_at(&unique_endpoint(), data_dir, downloads).await
    }

    pub async fn start_at(endpoint: &str, data_dir: &Path, downloads: &Path) -> Self {
        std::fs::create_dir_all(data_dir).unwrap();
        std::fs::create_dir_all(downloads).unwrap();
        write_settings(data_dir, downloads);

        let (stop, stopped) = oneshot::channel();
        let config = vortexd::Config {
            data_dir: data_dir.to_path_buf(),
            endpoint: endpoint.to_owned(),
            ephemeral: false,
            ..Default::default()
        };
        let task = tokio::spawn(vortexd::run_until(config, async {
            let _ = stopped.await;
        }));

        let daemon = Self {
            endpoint: endpoint.to_owned(),
            data_dir: data_dir.to_path_buf(),
            downloads: downloads.to_path_buf(),
            stop: Some(stop),
            task,
        };
        daemon.wait_until_listening().await;
        daemon
    }

    /// Waits for a daemon that answers, not merely for an endpoint that accepts. A pipe
    /// instance left behind by a stopped daemon will connect and then say nothing at all.
    async fn wait_until_listening(&self) {
        for _ in 0..200 {
            if let Ok(stream) = vortex_ipc::connect_at(&self.endpoint).await {
                let (mut reader, mut writer) = tokio::io::split(stream);
                let ping = codec::write_frame(&mut writer, &Command::Ping).await;
                if ping.is_ok() {
                    let pong = tokio::time::timeout(
                        Duration::from_millis(250),
                        codec::read_frame::<_, Event>(&mut reader),
                    )
                    .await;
                    if matches!(pong, Ok(Ok(Some(Event::Pong)))) {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the daemon never started listening on {}", self.endpoint);
    }

    pub async fn client(&self) -> Client {
        Client::connect(&self.endpoint).await
    }

    /// Stops the daemon the way logout does, and waits for it to finish winding down.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(PATIENCE, &mut self.task).await;
    }
}

pub fn write_settings(data_dir: &Path, downloads: &Path) {
    let settings = Settings {
        download_dir: downloads.to_string_lossy().into_owned(),
        // Small enough to exercise the queue in a test, large enough to be realistic.
        max_concurrent_jobs: 2,
        max_connections: 8,
        ..Settings::default()
    };
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::write(
        data_dir.join("settings.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
}

pub struct Client {
    reader: ReadHalf<ClientStream>,
    writer: WriteHalf<ClientStream>,
}

impl Client {
    /// Connects and completes the handshake, the way every real client starts.
    pub async fn connect(endpoint: &str) -> Self {
        let mut client = Self::connect_raw(endpoint).await;
        client
            .send(Command::Hello {
                client: "test".into(),
                protocol: vortex_proto::PROTOCOL_VERSION,
            })
            .await;
        client
            .expect(matching(|e| matches!(e, Event::Hello { .. })))
            .await;
        client
    }

    /// Connects without saying hello, for the tests that are about the handshake itself.
    pub async fn connect_raw(endpoint: &str) -> Self {
        let stream = vortex_ipc::connect_at(endpoint)
            .await
            .expect("connecting to the daemon");
        let (reader, writer) = tokio::io::split(stream);
        Self { reader, writer }
    }

    pub async fn send(&mut self, command: Command) {
        codec::write_frame(&mut self.writer, &command)
            .await
            .expect("sending a command");
    }

    pub async fn next(&mut self) -> Event {
        tokio::time::timeout(PATIENCE, codec::read_frame::<_, Event>(&mut self.reader))
            .await
            .expect("the daemon went quiet")
            .expect("reading an event")
            .expect("the daemon closed the connection")
    }

    /// Reads until an event matches, returning it. Every event seen on the way is
    /// returned too, because a test usually wants to assert on the sequence.
    pub async fn expect<T>(&mut self, mut want: impl FnMut(&Event) -> Option<T>) -> T
    where
        T: 'static,
    {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let event = tokio::time::timeout_at(deadline, self.next())
                .await
                .expect("gave up waiting for the event under test");
            if let Some(found) = want(&event) {
                return found;
            }
        }
    }

    /// Waits for a job to reach a state, and fails loudly if it reaches a terminal state
    /// that is not the one asked for.
    pub async fn wait_for_state(&mut self, job: JobId, is: impl Fn(&vortex_proto::JobState) -> bool) {
        self.expect(|event| match event {
            Event::JobStateChanged { job: id, state } if *id == job && is(state) => Some(()),
            _ => None,
        })
        .await;
    }
}

/// A predicate helper: `expect(matching(|e| ...))` reads better than a bare closure at the
/// call sites that only care whether an event happened.
pub fn matching(f: impl Fn(&Event) -> bool) -> impl Fn(&Event) -> Option<()> {
    move |event| f(event).then_some(())
}

impl Client {
    pub async fn submit(&mut self, spec: vortex_proto::JobSpec) -> JobId {
        self.send(Command::Submit { spec }).await;
        self.expect(|event| match event {
            Event::JobAdded { job } => Some(job.id),
            _ => None,
        })
        .await
    }
}
