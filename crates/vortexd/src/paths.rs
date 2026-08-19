//! Where the daemon keeps its own state (01 §State ownership).
//!
//! ```text
//! %LOCALAPPDATA%\Vortex\                    (Windows)
//! ~/.local/share/vortex/                    (Linux)
//! ~/Library/Application Support/Vortex/     (macOS)
//! ├── vortex.db      SQLite WAL — jobs, history, per-origin policy
//! ├── settings.json  human-editable
//! └── logs/          rotating, structured
//! ```
//!
//! Partial downloads deliberately do **not** live here — they sit next to the destination
//! file, so a half-finished download survives being moved to another drive.

use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    // Capitalised on Windows and macOS, lowercase on Linux — each platform's convention,
    // since this directory is visible to the user.
    if cfg!(target_os = "linux") {
        base.join("vortex")
    } else {
        base.join("Vortex")
    }
}

pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("vortex.db")
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

pub fn logs_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// The user's downloads folder — a known folder on Windows and macOS, so a relocated
/// `Downloads` is honoured rather than guessed at.
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
}
