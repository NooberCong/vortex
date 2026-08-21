//! Starting one of Vortex's own executables from another.
//!
//! Two directions use this. `vortex-host` and `vortex-app` start the **daemon** when they
//! find nothing listening: either can be the first thing to want Vortex after a login,
//! before the autostart entry has run. The daemon starts the **app** when a `Reveal`
//! arrives from the extension, which is a browser asking for a window in a process that
//! has none.
//!
//! Every one of those spawns has to be identical in two details that are easy to get
//! subtly wrong in one copy and not the other: the target is resolved from beside the
//! calling executable rather than through `PATH`, and on Windows it is spawned with
//! `CREATE_NO_WINDOW` so starting a download never flashes a console at the user. So
//! there is one [`spawn_beside`], and the named helpers are one line each.

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
pub fn start() -> std::io::Result<()> {
    spawn_beside("vortexd", &[])
}

/// Spawns `vortex-app`, optionally pointed at one job, and returns immediately.
///
/// The app's own single-instance plugin decides what a second copy means: if a window is
/// already open, these arguments are handed to it and this process exits without ever
/// drawing anything. That is why the daemon does not have to know whether the app is
/// running — a question it has no way to answer, since the app connects over the same
/// anonymous pipe every other client does.
pub fn open_app(args: &[&str]) -> std::io::Result<()> {
    spawn_beside("vortex-app", args)
}

/// Starts a sibling executable: the one beside this process, detached, and silent.
///
/// Resolving through `PATH` would be a way for something else to be started instead, and
/// the pieces of an installed Vortex always ship in one directory.
pub fn spawn_beside(stem: &str, args: &[&str]) -> std::io::Result<()> {
    let program = beside_current_exe(stem)?;
    if !program.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no {stem} at {}", program.display()),
        ));
    }

    let mut command = std::process::Command::new(program);
    command
        .args(args)
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

fn beside_current_exe(stem: &str) -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "the client is not in a directory")
    })?;
    Ok(dir.join(if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_is_looked_for_beside_the_caller_and_nowhere_else() {
        let path = beside_current_exe("vortexd").unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(path.parent(), exe.parent());
        assert!(path.file_stem().is_some_and(|name| name == "vortexd"), "{path:?}");
    }

    #[test]
    fn a_missing_daemon_is_not_found_rather_than_a_spawn_failure() {
        // The test binary lives in `target/debug/deps`, where no `vortexd` is ever built,
        // so this exercises the branch a broken install would take. A `NotFound` is what
        // lets a caller say "Vortex isn't running" instead of surfacing an OS error code.
        if beside_current_exe("vortexd").unwrap().exists() {
            return;
        }
        let err = start().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
