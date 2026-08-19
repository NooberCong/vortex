//! `vortexd` — the process that owns every job, every byte and every setting.
//!
//! It is a **user-session background process**, not a service: no admin rights, no Session
//! 0 isolation, and therefore the user's own proxy settings, certificate store and network
//! context — which is what makes it work behind a corporate MITM proxy (01 §Process
//! model).
//!
//! The binary is a thin wrapper around [`run`]; everything here is driveable from a test.

pub mod daemon;
mod frames;
mod jobs;
pub mod paths;
mod rehydrate;
mod server;
pub mod settings;
pub mod store;

pub use daemon::{Config, Daemon, Msg};

use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a stop waits for running transfers to wind down before the process leaves
/// anyway. Everything durable is written continuously, so the cost of giving up here is a
/// few re-fetched blocks, not a corrupted file.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Binds the endpoint, restores the list, and runs until the OS asks the process to stop.
pub async fn run(config: Config) -> anyhow::Result<()> {
    run_until(config, shutdown_signal()).await
}

/// The same, stopping when `shutdown` resolves. Tests hold that future; the binary hands
/// over the OS signal.
pub async fn run_until(
    config: Config,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    // Binding first is the single-instance check: the endpoint is the lock.
    let listener = match vortex_ipc::Listener::bind_at(&config.endpoint) {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::info!("Vortex is already running");
            return Ok(());
        }
        Err(e) => return Err(anyhow::Error::new(e).context("binding the Vortex endpoint")),
    };
    tracing::info!(endpoint = %config.endpoint, "listening");

    // Only now, and only here: binding is what makes this process *the* daemon, and
    // reconciling from a copy that is about to exit because another one already holds the
    // endpoint would rewrite a working install with whatever this copy's paths happen to
    // be.
    if config.os_integration {
        integrate(&config.data_dir);
    }

    let (tx, rx) = mpsc::channel(1024);
    let daemon = Daemon::new(&config, tx.clone())?;
    let running = tokio::spawn(daemon.run(rx));
    let accepting = tokio::spawn(server::serve(listener, tx.clone()));

    shutdown.await;
    tracing::info!("stopping");

    let (done, stopped) = oneshot::channel();
    if tx.send(Msg::Shutdown(done)).await.is_ok() {
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, stopped).await;
    }
    accepting.abort();
    // Await the abort: the listener owns the endpoint, and until the task is really gone
    // a daemon started immediately afterwards would find the address still taken.
    let _ = accepting.await;
    let _ = running.await;
    Ok(())
}

/// Re-registers the native-messaging manifests, every start.
///
/// This is a reconciliation rather than an install step, and the case it exists for is
/// ordinary: a browser installed after Vortex was. Without it that browser has no manifest,
/// so the extension can never reach the daemon, and the extension is deliberately silent
/// about being unable to (03 §2) — the user sees a download manager that does nothing.
///
/// Failures are logged and dropped. Not being able to register Vivaldi is not a reason to
/// refuse to run.
fn integrate(data_dir: &std::path::Path) {
    let registration = match vortex_setup::Registration::new(data_dir) {
        Ok(registration) => registration,
        Err(e) => return tracing::warn!("cannot work out where Vortex is installed: {e}"),
    };
    for outcome in vortex_setup::register(&registration) {
        match outcome.state {
            vortex_setup::State::Written => tracing::info!(browser = outcome.browser, "registered"),
            vortex_setup::State::Failed(e) => {
                tracing::warn!(browser = outcome.browser, "registration failed: {e}")
            }
            vortex_setup::State::NoIdentity => tracing::warn!(
                browser = outcome.browser,
                "registered, but no extension id is pinned into this build"
            ),
            _ => {}
        }
    }
}

/// `Ctrl-C`, and on unix `SIGTERM` as well — logout sends the latter, and a download
/// manager that loses state at logout is not a download manager.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
