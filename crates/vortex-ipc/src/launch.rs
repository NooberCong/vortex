//! Starting the daemon, for the two clients that may find it not running.
//!
//! `vortex-host` is started by a browser that has just seen a download; `vortex-app` is
//! started by a user double-clicking. Either can be the first thing to want Vortex after a
//! login, before the autostart entry has run — so both need to be able to bring the daemon
//! up, and both need to do it *identically*. The two details worth sharing are easy to get
//! subtly wrong in one copy and not the other: the daemon is resolved from beside the
//! calling executable rather than through `PATH`, and on Windows it is spawned with
//! `CREATE_NO_WINDOW` so starting a download never flashes a console at the user.

use std::path::PathBuf;
use std::time::Duration;

/// How long to wait for a daemon we just started. A cold `vortexd` binds its endpoint
/// before it does anything else, so this is generous.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// Connects, starting `vortexd` if nothing is listening.
///
/// A failure here means "not running and would not start", which every caller treats the
/// same way: say so plainly and do nothing else. The extension goes passive rather than
/// eating the user's download (03 §2); the app shows its disconnected state rather than an
/// empty list that looks like a lost queue.
pub async fn connect_or_start() -> std::io::Result<crate::ClientStream> {
    if let Ok(stream) = crate::connect().await {
        return Ok(stream);
    }
    start()?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        match crate::connect().await {
            Ok(stream) => return Ok(stream),
            Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Spawns `vortexd` from beside the current executable and returns immediately.
///
/// Resolving through `PATH` would be a way for something else to be started instead, and
/// an installed client and an installed daemon always ship in the same directory.
pub fn start() -> std::io::Result<()> {
    let daemon = beside_current_exe()?;
    if !daemon.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no daemon at {}", daemon.display()),
        ));
    }

    let mut command = std::process::Command::new(daemon);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()?;
    Ok(())
}

fn beside_current_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "the client is not in a directory")
    })?;
    Ok(dir.join(if cfg!(windows) { "vortexd.exe" } else { "vortexd" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_is_looked_for_beside_the_caller_and_nowhere_else() {
        let path = beside_current_exe().unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(path.parent(), exe.parent());
        assert!(path.file_stem().is_some_and(|name| name == "vortexd"), "{path:?}");
    }

    #[test]
    fn a_missing_daemon_is_not_found_rather_than_a_spawn_failure() {
        // The test binary lives in `target/debug/deps`, where no `vortexd` is ever built,
        // so this exercises the branch a broken install would take. A `NotFound` is what
        // lets a caller say "Vortex isn't running" instead of surfacing an OS error code.
        if beside_current_exe().unwrap().exists() {
            return;
        }
        let err = start().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
