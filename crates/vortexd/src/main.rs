//! The `vortexd` binary. Everything it does lives in the library; this is the front door.

// Windowless on Windows, because the login entry hands this path to Explorer and Explorer
// gives a console-subsystem program a console: a black window sitting on the user's desktop
// for as long as the daemon lives, which is the whole session. Nothing is lost by it — the
// daemon's output is a log file (`install_logging`), never a terminal, and the two commands
// that do print borrow the caller's console instead (`attach_parent_console`).
//
// Debug builds keep theirs, so that running one from a terminal behaves the way a developer
// expects.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

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
    /// Register the native-messaging manifests with every browser, set the login entry to
    /// whatever `settings.json` asks for, and exit. The installer runs this; so can anyone
    /// whose capture has gone quiet or whose Vortex stopped starting at sign-in.
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
    // Before `--help`, `--version` and `--register` have anything to say.
    #[cfg(windows)]
    attach_parent_console();

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

    // The login entry is set here, on both sides, rather than left to the daemon.
    //
    // Leaving installs to the daemon looks tidier and is wrong, because an install is
    // preceded by an *un*install: Tauri's NSIS runs the old uninstaller before copying
    // anything, on an update and on a plain reinstall over an existing copy. That takes
    // the entry with it, `--register` used to put nothing back, and the only thing left
    // that would have was a daemon start — which is exactly what the missing entry
    // prevents at the next sign-in. The symptom is a machine that comes back from a
    // restart with the setting still reading "on" and nothing running.
    //
    // A first install has no settings file and gets the default, which is on: "installed"
    // and "starts when I sign in" are the same thing to anyone who did not go looking for
    // a toggle, and the toggle is one screen away for anyone who did.
    let settings = vortexd::paths::settings_path(data_dir);
    let enabled = register && vortexd::settings::autostart_intent(&settings);
    match vortex_setup::set_autostart(enabled) {
        Ok(()) => println!("{:<12} {}", "login entry", if enabled { "added" } else { "removed" }),
        Err(e) => println!("{:<12} FAILED: {e}", "login entry"),
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

/// Points the standard handles at the console that started us, if a console started us.
///
/// The binary is windowless (see the top of the file), and Windows reads that as "this one
/// never wants a console" — including when a person types `vortexd --register` at a prompt,
/// where the report it prints would go nowhere at all. Borrowing the parent's console puts
/// that output back in front of the person who asked for it.
///
/// A handle the parent already supplied is never replaced. The installer runs `--register`
/// through a pipe and that pipe is what the install log is made of; overwriting it with the
/// console would empty the log and put the text on a desktop nobody is looking at.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_HANDLE, STD_OUTPUT_HANDLE,
    };

    // Asked before attaching, because attaching is one of the things that can fill them in.
    let missing = |id: STD_HANDLE| {
        let handle = unsafe { GetStdHandle(id) };
        handle.is_null() || handle == INVALID_HANDLE_VALUE
    };
    let (no_stdout, no_stderr) = (missing(STD_OUTPUT_HANDLE), missing(STD_ERROR_HANDLE));
    if !no_stdout && !no_stderr {
        return;
    }
    // Fails when there is no parent console — a login-time start, most of the time. That is
    // the ordinary case, not an error: the daemon has nothing to say to a terminal anyway.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }

    let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
    let console: HANDLE = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if console == INVALID_HANDLE_VALUE {
        return;
    }
    for (id, needed) in [(STD_OUTPUT_HANDLE, no_stdout), (STD_ERROR_HANDLE, no_stderr)] {
        if needed {
            unsafe { SetStdHandle(id, console) };
        }
    }
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
