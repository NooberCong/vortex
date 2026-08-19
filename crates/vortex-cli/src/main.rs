//! `vortex` — the command line.
//!
//! Two jobs, kept apart on purpose:
//!
//! * Every subcommand except `fetch` is an **ordinary client** of the daemon, speaking the
//!   same protocol as the desktop app. If the CLI can do something the app cannot, that is
//!   a missing button, not a missing command.
//! * `fetch` runs the engine **in this process**, with no daemon at all. That is the
//!   harness phase 1 was built against, and it stays because "is it the engine or is it
//!   the daemon?" should take one command to answer.

mod fetch;
mod media;
mod render;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::io::{ReadHalf, WriteHalf};
use vortex_ipc::ClientStream;
use vortex_proto::codec;
use vortex_proto::{Command as Cmd, Event, JobId, JobSpec, Priority, RequestEnvelope, SubscriptionScope};

#[derive(Parser)]
#[command(name = "vortex", version, about = "The Vortex download manager")]
struct Args {
    #[command(subcommand)]
    command: Action,
    /// Talk to a daemon on a different endpoint. For development.
    #[arg(long, global = true)]
    endpoint: Option<String>,
}

#[derive(Subcommand)]
enum Action {
    /// Queue a download.
    Add {
        url: String,
        /// Where to save it. Defaults to the configured folder for its category.
        #[arg(long)]
        dir: Option<String>,
        /// Save it under this name instead of the one the server suggests.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        paused: bool,
        #[arg(long, value_parser = ["low", "normal", "high"], default_value = "normal")]
        priority: String,
    },
    /// List every job the daemon knows about.
    #[command(alias = "ls")]
    List,
    Pause { job: u64 },
    Resume { job: u64 },
    Cancel { job: u64 },
    /// Take a job out of the list.
    #[command(alias = "rm")]
    Remove {
        job: u64,
        /// Also delete the file it produced.
        #[arg(long)]
        delete_file: bool,
    },
    /// Follow progress until interrupted.
    Watch,
    /// Ask what a URL is, without downloading it.
    Probe { url: String },
    /// Print the current settings as JSON.
    Settings,
    /// Read a stream's ladder, or download one, without a daemon. The media harness.
    Media {
        /// A manifest URL, or a page URL for the yt-dlp fallback.
        url: String,
        /// Download this variant, by the id shown in the listing.
        #[arg(long)]
        variant: Option<String>,
        /// Download the variant nearest this height.
        #[arg(long)]
        height: Option<u32>,
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        name: Option<String>,
        /// Leave subtitle tracks out of the file.
        #[arg(long)]
        no_subtitles: bool,
        #[arg(long, value_parser = ["auto", "mp4", "mkv"], default_value = "auto")]
        container: String,
        #[arg(long, default_value_t = 16)]
        connections: u8,
    },
    /// Download without a daemon, straight to a file. The engine harness.
    Fetch {
        url: String,
        #[arg(long, default_value = ".")]
        dir: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 16)]
        connections: u8,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    // The two harness commands run the engine here rather than asking a daemon.
    if let Action::Fetch {
        url,
        dir,
        name,
        connections,
    } = &args.command
    {
        return fetch::run(url, dir, name.clone(), *connections).await;
    }
    if let Action::Media {
        url,
        variant,
        height,
        dir,
        name,
        no_subtitles,
        container,
        connections,
    } = &args.command
    {
        return media::run(media::Options {
            url: url.clone(),
            variant: variant.clone(),
            height: *height,
            dir: dir.clone(),
            name: name.clone(),
            subtitles: !no_subtitles,
            container: match container.as_str() {
                "mp4" => vortex_proto::ContainerPreference::Mp4,
                "mkv" => vortex_proto::ContainerPreference::Mkv,
                _ => vortex_proto::ContainerPreference::Auto,
            },
            connections: *connections,
        })
        .await;
    }

    let mut client = Client::connect(args.endpoint.as_deref()).await?;
    match args.command {
        Action::Add {
            url,
            dir,
            name,
            paused,
            priority,
        } => {
            let spec = JobSpec {
                dest_dir: dir,
                filename: name,
                start_paused: paused,
                priority: match priority.as_str() {
                    "low" => Priority::Low,
                    "high" => Priority::High,
                    _ => Priority::Normal,
                },
                ..JobSpec::file(RequestEnvelope::new(url))
            };
            client.send(Cmd::Submit { spec }).await?;
            let view = client
                .expect(|event| match event {
                    Event::JobAdded { job } => Some(job.clone()),
                    _ => None,
                })
                .await?;
            println!("{}", render::row(&view));
        }
        Action::List => {
            client.send(Cmd::List).await?;
            let jobs = client
                .expect(|event| match event {
                    Event::Jobs { jobs } => Some(jobs.clone()),
                    _ => None,
                })
                .await?;
            if jobs.is_empty() {
                println!("Nothing here yet.");
            }
            for job in &jobs {
                println!("{}", render::row(job));
            }
        }
        Action::Pause { job } => client.act(Cmd::Pause { job: JobId(job) }, JobId(job)).await?,
        Action::Resume { job } => client.act(Cmd::Resume { job: JobId(job) }, JobId(job)).await?,
        Action::Cancel { job } => client.act(Cmd::Cancel { job: JobId(job) }, JobId(job)).await?,
        Action::Remove { job, delete_file } => {
            client
                .act(
                    Cmd::Remove {
                        job: JobId(job),
                        delete_file,
                    },
                    JobId(job),
                )
                .await?
        }
        Action::Watch => client.watch().await?,
        Action::Probe { url } => {
            client
                .send(Cmd::Probe {
                    envelope: RequestEnvelope::new(url),
                })
                .await?;
            let line = client
                .expect(|event| match event {
                    Event::Probed { result } => Some(render::probe(result)),
                    Event::Error { message } => Some(message.clone()),
                    _ => None,
                })
                .await?;
            println!("{line}");
        }
        Action::Settings => {
            client.send(Cmd::GetSettings).await?;
            let settings = client
                .expect(|event| match event {
                    Event::SettingsChanged { settings } => Some(settings.clone()),
                    _ => None,
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        Action::Fetch { .. } | Action::Media { .. } => {
            unreachable!("the harness commands are handled before connecting")
        }
    }
    Ok(())
}

struct Client {
    reader: ReadHalf<ClientStream>,
    writer: WriteHalf<ClientStream>,
}

impl Client {
    async fn connect(endpoint: Option<&str>) -> Result<Self> {
        let stream = match endpoint {
            Some(endpoint) => vortex_ipc::connect_at(endpoint).await,
            None => vortex_ipc::connect().await,
        }
        .map_err(|_| anyhow::anyhow!("Vortex isn't running. Start it with `vortexd`."))?;

        let (reader, writer) = tokio::io::split(stream);
        let mut client = Self { reader, writer };
        client
            .send(Cmd::Hello {
                client: format!("vortex-cli/{}", env!("CARGO_PKG_VERSION")),
                protocol: vortex_proto::PROTOCOL_VERSION,
            })
            .await?;
        client
            .expect(|event| match event {
                Event::Hello { .. } => Some(()),
                _ => None,
            })
            .await?;
        Ok(client)
    }

    async fn send(&mut self, command: Cmd) -> Result<()> {
        codec::write_frame(&mut self.writer, &command).await?;
        Ok(())
    }

    async fn next(&mut self) -> Result<Event> {
        codec::read_frame::<_, Event>(&mut self.reader)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the daemon closed the connection"))
    }

    /// Reads until something answers the question that was asked. An `Error` event always
    /// answers it, because the daemon says why in a sentence.
    async fn expect<T>(&mut self, mut want: impl FnMut(&Event) -> Option<T>) -> Result<T> {
        loop {
            let event = self.next().await?;
            if let Some(found) = want(&event) {
                return Ok(found);
            }
            if let Event::Error { message } = event {
                anyhow::bail!(message);
            }
        }
    }

    /// Sends a command that changes one job, and waits for the daemon to confirm what it
    /// did rather than exiting into uncertainty.
    async fn act(&mut self, command: Cmd, job: JobId) -> Result<()> {
        self.send(command).await?;
        let line = self
            .expect(|event| match event {
                Event::JobStateChanged { job: id, state } if *id == job => {
                    Some(render::state(state).to_owned())
                }
                Event::JobRemoved { job: id } if *id == job => Some("removed".to_owned()),
                _ => None,
            })
            .await?;
        println!("{}  {line}", job.0);
        Ok(())
    }

    /// The list, live. Summary scope is the cheap one — 2 Hz, no per-worker detail — which
    /// is exactly what a terminal can render (01 §IPC).
    async fn watch(&mut self) -> Result<()> {
        self.send(Cmd::Subscribe {
            scope: SubscriptionScope::Summary,
        })
        .await?;
        self.send(Cmd::List).await?;
        loop {
            match self.next().await? {
                Event::Jobs { jobs } => {
                    for job in &jobs {
                        println!("{}", render::row(job));
                    }
                }
                Event::JobAdded { job } => println!("{}", render::row(&job)),
                Event::JobSummary { job, frame } => {
                    println!("{}", render::summary(job, &frame))
                }
                Event::JobStateChanged { job, state } => {
                    println!("{}  {}", job.0, render::state(&state))
                }
                Event::JobFinished { job, outcome } => {
                    println!("{}  {}", job.0, render::outcome(&outcome))
                }
                Event::Error { message } => eprintln!("{message}"),
                _ => {}
            }
        }
    }
}
