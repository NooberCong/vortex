//! The `vortexd` binary. Everything it does lives in the library; this is the front door.

use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::prelude::*;
use vortexd::Config;

#[derive(Parser)]
#[command(name = "vortexd", version, about = "The Vortex download daemon")]
struct Args {
    /// Where the job list, settings and logs live. Defaults to the per-user data
    /// directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Listen somewhere other than the per-user endpoint. For development and tests.
    #[arg(long)]
    endpoint: Option<String>,
    /// Keep the job list in memory only — nothing is written to `vortex.db`.
    #[arg(long)]
    ephemeral: bool,
    /// Log to stderr as well as to the log directory.
    #[arg(long)]
    foreground: bool,
    /// `error`, `warn`, `info`, `debug`, `trace`, or any `RUST_LOG` filter.
    #[arg(long, default_value = "info")]
    log: String,
    /// Register the native-messaging manifests with every browser and exit. The installer
    /// runs this; so can anyone whose capture has gone quiet.
    #[arg(long, conflicts_with = "unregister")]
    register: bool,
    /// Remove every manifest and the login entry, and exit. The uninstaller runs this.
    #[arg(long)]
    unregister: bool,
    /// Do not touch the registry, the login entry, or any browser's configuration.
    #[arg(long)]
    no_integrate: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let data_dir = args.data_dir.unwrap_or_else(vortexd::paths::data_dir);

    // Before logging is installed, because these two run from an installer and their
    // output belongs on stdout where the installer log can capture it.
    if args.register || args.unregister {
        return integrate(&data_dir, args.register);
    }

    let _logs = install_logging(&data_dir, &args.log, args.foreground);

    let config = Config {
        endpoint: match args.endpoint {
            Some(endpoint) => endpoint,
            None => vortex_ipc::endpoint()?,
        },
        data_dir,
        ephemeral: args.ephemeral,
        // An ephemeral daemon keeps nothing, so it has no business editing the machine
        // either — that is the mode a developer runs two of.
        os_integration: !args.ephemeral && !args.no_integrate,
    };

    // The runtime is sized to the machine, and the writer threads sit outside it — a
    // blocking write on a slow disk must never occupy a runtime worker (01 §Threading).
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(vortexd::run(config))
}

/// `--register` / `--unregister`, for the installer and for anyone whose capture has gone
/// quiet.
///
/// Registration succeeds per browser, so this reports rather than aborts — and it exits
/// non-zero only if *every* browser failed, because an installer that fails the whole
/// install over one browser with a locked registry key is worse than the problem.
fn integrate(data_dir: &std::path::Path, register: bool) -> anyhow::Result<()> {
    // Warnings from the registration itself — a malformed `VORTEX_EXTENSION_ID`, most
    // usefully — are the whole reason someone runs this by hand. Without a subscriber they
    // are dropped, and the command silently does the wrong thing correctly.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .without_time()
        .with_target(false)
        .init();

    let registration = vortex_setup::Registration::new(data_dir)?;
    let outcomes = if register {
        vortex_setup::register(&registration)
    } else {
        vortex_setup::unregister(&registration)
    };
    print!("{}", vortex_setup::summarise(&outcomes));

    // The login entry is Vortex's own, so uninstalling takes it and installing leaves it
    // to the setting — a fresh install has no settings file yet, and writing the entry
    // here would install a startup item the user was never asked about.
    if !register {
        match vortex_setup::set_autostart(false, &registration.daemon) {
            Ok(()) => println!("{:<12} removed", "login entry"),
            Err(e) => println!("{:<12} FAILED: {e}", "login entry"),
        }
    }

    let failed = outcomes
        .iter()
        .filter(|o| matches!(o.state, vortex_setup::State::Failed(_)))
        .count();
    if failed == outcomes.len() {
        anyhow::bail!("no browser could be reached");
    }
    Ok(())
}

/// Rotating JSON to `logs/`, plus human-readable stderr when asked. Returns the guard that
/// keeps the writer thread alive.
fn install_logging(
    data_dir: &std::path::Path,
    filter: &str,
    foreground: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter));

    let logs = vortexd::paths::logs_dir(data_dir);
    let file = std::fs::create_dir_all(&logs).ok().and_then(|_| {
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("vortexd")
            .filename_suffix("log")
            // Seven days is the retention promised in 01 §State ownership.
            .max_log_files(7)
            .build(&logs)
            .ok()
    });

    let (writer, guard) = match file {
        Some(file) => {
            let (writer, guard) = tracing_appender::non_blocking(file);
            (Some(writer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(writer.map(|writer| {
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(writer)
                .with_ansi(false)
        }))
        .with(foreground.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr)))
        .init();

    guard
}
