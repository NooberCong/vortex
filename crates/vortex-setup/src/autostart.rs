//! "Start Vortex when I sign in".
//!
//! The setting in `settings.json` is the user's *intent*; the OS entry is the fact. They
//! are reconciled in one direction only — intent wins — because the setting is the thing
//! the user edits and the entry is the thing they never see. The cost of that choice is
//! that removing the entry by hand (Task Manager's Startup tab, say) is undone the next
//! time the daemon reconciles, which is why the settings toggle is the one place the UI
//! puts it.
//!
//! Two callers reconcile, and both have to: the installer, because an update uninstalls
//! before it installs and something has to put the entry back before the next sign-in;
//! and the daemon, because a browser installed later needs registration anyway and the
//! entry may have gone missing since.
//!
//! Nothing here starts or stops anything. Turning autostart on does not launch a daemon,
//! and turning it off does not kill one — both would be surprising, and the daemon that
//! would be affected is the one running the code.
//!
//! ## What the entry starts
//!
//! `vortex-app --tray`, not `vortexd`. The daemon is the process that has to be running,
//! and for a while the entry named it directly — correct, and unusable: a headless process
//! has nothing in the tray, nothing in the taskbar and no window, so a user who signs in
//! has no way to tell Vortex from Vortex-having-failed-to-start. The app in tray mode opens
//! no window either, but it leaves an icon that says the thing is there, with the aggregate
//! throughput in its tooltip.
//!
//! One entry still gets both, because the app starts the daemon on its way up the same way
//! a double-click does (`vortex_ipc::connect_or_start`). Quitting the app afterwards leaves
//! the daemon running, which is the same promise the tray's Quit has always made.

use std::path::PathBuf;

/// What the login entry passes to `vortex-app`: the tray icon, and no window.
///
/// `apps/desktop/src-tauri/src/lib.rs` reads this argument and the two spellings have to
/// agree. There is no crate to share it through — the app does not depend on this one, and
/// this one deliberately depends on nothing of Vortex's — so it is written twice, with each
/// side naming the other.
pub const TRAY_FLAG: &str = "--tray";

/// Adds or removes the login entry. Idempotent: an entry that already says the right thing
/// is left entirely alone, mtime included.
pub fn set_autostart(enabled: bool) -> std::io::Result<()> {
    // Only adding needs a path — removing is by name, so a machine where `current_exe` has
    // somehow failed can still turn the entry off.
    let program = if enabled { login_program()? } else { PathBuf::new() };
    crate::platform::set_autostart(enabled, &program)
}

/// `vortex-app`, beside whoever is asking.
///
/// Beside-the-caller for the reason [`crate::Registration::new`] gives — the pieces are
/// installed together and live in one directory — and absolute because a login entry has no
/// working directory to be relative to.
fn login_program() -> std::io::Result<PathBuf> {
    let dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "the caller is not in a directory")
        })?
        .to_path_buf();
    Ok(dir.join(crate::exe("vortex-app")))
}

/// Whether the OS would start Vortex at login right now.
///
/// This is the fact rather than the intent, and the two can disagree — a user who deleted
/// the entry by hand, an update whose uninstall step took it, a profile copied between
/// machines. It is what the daemon compares the setting against before deciding it has
/// nothing to do; it is never reconciled backwards into the setting.
pub fn autostart_enabled() -> bool {
    crate::platform::autostart_enabled()
}
