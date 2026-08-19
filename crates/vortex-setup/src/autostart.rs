//! "Start Vortex when I sign in".
//!
//! The setting in `settings.json` is the user's *intent*; the OS entry is the fact. They
//! are reconciled in one direction only — intent wins — because the setting is the thing
//! the user edits and the entry is the thing they never see. The cost of that choice is
//! that removing the entry by hand (Task Manager's Startup tab, say) is undone the next
//! time the daemon starts, which is why the settings toggle is the one place the UI puts
//! it.
//!
//! Nothing here starts or stops anything. Turning autostart on does not launch a daemon,
//! and turning it off does not kill one — both would be surprising, and the daemon that
//! would be affected is the one running the code.

use std::path::Path;

/// Adds or removes the login entry. Idempotent: an entry that already says the right thing
/// is left entirely alone, mtime included.
pub fn set_autostart(enabled: bool, daemon: &Path) -> std::io::Result<()> {
    crate::platform::set_autostart(enabled, daemon)
}

/// Whether the OS would start Vortex at login right now.
///
/// This is the fact rather than the intent, and the two can disagree — a user who deleted
/// the entry by hand, a profile copied between machines, an install that never ran. It is
/// reported rather than reconciled backwards.
pub fn autostart_enabled() -> bool {
    crate::platform::autostart_enabled()
}
