//! `settings.json` — human-editable, and re-read when it changes on disk.
//!
//! The file is the user's, not the daemon's: someone who edits it in a text editor should
//! see the change take effect without restarting anything. The daemon re-reads it whenever
//! it is already awake (a client connects, or a job is running). A fully idle daemon does
//! not stat the file on a timer, because "does not poll when idle" is a promise made in
//! 01 §Process model and a settings file is not worth breaking it for.

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use vortex_proto::{Category, Settings};

pub struct SettingsFile {
    path: PathBuf,
    current: Settings,
    seen: Option<SystemTime>,
}

impl SettingsFile {
    /// Loads the file, creating it with machine-appropriate defaults when it is missing.
    /// A malformed file is reported and then ignored — the daemon starting with defaults
    /// is much better than the daemon refusing to start.
    pub fn load(path: &Path) -> Self {
        let mut file = Self {
            path: path.to_owned(),
            current: defaults(),
            seen: None,
        };
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(settings) => {
                    file.current = normalize(settings);
                    file.seen = mtime(path);
                }
                Err(e) => tracing::warn!(path = %path.display(), "settings.json is not valid: {e} — using defaults"),
            },
            Err(_) => {
                let _ = file.save();
            }
        }
        file
    }

    pub fn get(&self) -> &Settings {
        &self.current
    }

    pub fn set(&mut self, settings: Settings) -> &Settings {
        self.current = normalize(settings);
        let _ = self.save();
        &self.current
    }

    /// Re-reads the file if it changed since we last looked. Returns the new settings only
    /// when they actually differ, so a touched-but-unchanged file does not spam clients.
    pub fn reload_if_changed(&mut self) -> Option<&Settings> {
        let stamp = mtime(&self.path)?;
        if self.seen == Some(stamp) {
            return None;
        }
        self.seen = Some(stamp);
        let text = std::fs::read_to_string(&self.path).ok()?;
        let parsed = match serde_json::from_str::<Settings>(&text) {
            Ok(s) => normalize(s),
            Err(e) => {
                tracing::warn!("settings.json changed but is not valid: {e} — keeping the previous settings");
                return None;
            }
        };
        if parsed == self.current {
            return None;
        }
        self.current = parsed;
        Some(&self.current)
    }

    fn save(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.current).unwrap_or_default();
        std::fs::write(&self.path, text)?;
        self.seen = mtime(&self.path);
        Ok(())
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Whether the login entry should exist, answered from the file alone.
///
/// `--register` asks this before any daemon has run, which is why it does not go through
/// [`SettingsFile`]: loading writes the file when it is missing, and a `--register` that
/// only means to look should not leave a settings file behind on a machine where the user
/// has not yet opened Vortex.
///
/// No file means a first install, and the answer there is the shipped default — on, which
/// is what the toggle in Settings shows from the first time it is opened.
pub fn autostart_intent(path: &Path) -> bool {
    let fallback = Settings::default().autostart;
    let Ok(text) = std::fs::read_to_string(path) else {
        return fallback;
    };
    match serde_json::from_str::<Settings>(&text) {
        Ok(settings) => settings.autostart,
        Err(e) => {
            // Onto the installer's log, where someone reading it can see why an install
            // turned the login entry on despite a file that says otherwise.
            tracing::warn!(path = %path.display(), "settings.json is not valid: {e} — assuming the default");
            fallback
        }
    }
}

fn defaults() -> Settings {
    Settings {
        download_dir: crate::paths::default_download_dir().to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

/// Repairs the values a hand-edited file can plausibly get wrong. Clamping beats refusing:
/// `maxConnections: 0` should mean "one connection", not "no downloads today".
fn normalize(mut settings: Settings) -> Settings {
    if settings.download_dir.trim().is_empty() {
        settings.download_dir = defaults().download_dir;
    }
    settings.max_connections = settings.max_connections.clamp(1, 32);
    settings.max_concurrent_jobs = settings.max_concurrent_jobs.clamp(1, 32);
    settings.write_buffer_budget = settings.write_buffer_budget.clamp(8 << 20, 2 << 30);
    settings.default_media_height = settings.default_media_height.clamp(144, 4320);
    settings
}

/// Where a job of this category should land. A category directory may be absolute (the
/// user pointing video at a second drive) or relative to the download directory.
pub fn dest_dir(settings: &Settings, category: Category) -> PathBuf {
    let root = PathBuf::from(&settings.download_dir);
    match settings.category_dirs.get(category.label()) {
        Some(dir) if !dir.trim().is_empty() => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                root.join(dir)
            }
        }
        _ => root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_edited_file_is_repaired_rather_than_rejected() {
        let mut settings = Settings {
            max_connections: 0,
            max_concurrent_jobs: 200,
            download_dir: "  ".into(),
            ..Settings::default()
        };
        settings = normalize(settings);
        assert_eq!(settings.max_connections, 1);
        assert_eq!(settings.max_concurrent_jobs, 32);
        assert!(!settings.download_dir.is_empty());
    }

    #[test]
    fn a_category_directory_may_be_absolute_or_relative() {
        let mut settings = defaults();
        settings.download_dir = if cfg!(windows) { r"C:\Downloads".into() } else { "/downloads".into() };
        settings
            .category_dirs
            .insert("Video".into(), "Films".into());
        assert_eq!(
            dest_dir(&settings, Category::Video),
            PathBuf::from(&settings.download_dir).join("Films")
        );
        assert_eq!(
            dest_dir(&settings, Category::Audio),
            PathBuf::from(&settings.download_dir)
        );

        let absolute = if cfg!(windows) { r"D:\Video" } else { "/mnt/video" };
        settings
            .category_dirs
            .insert("Video".into(), absolute.into());
        assert_eq!(dest_dir(&settings, Category::Video), PathBuf::from(absolute));
    }

    /// What `--register` reads during an install, when there may be no daemon, no file and
    /// no user to ask.
    #[test]
    fn the_login_entry_defaults_to_on_and_the_file_is_the_only_thing_that_turns_it_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // A first install: on, and looking must not create the file.
        assert!(autostart_intent(&path));
        assert!(!path.exists());

        std::fs::write(&path, r#"{"autostart": false}"#).unwrap();
        assert!(!autostart_intent(&path));

        std::fs::write(&path, r#"{"autostart": true}"#).unwrap();
        assert!(autostart_intent(&path));

        // A file someone edited badly is not a reason to leave a machine that used to
        // start Vortex no longer starting it.
        std::fs::write(&path, "{ not json").unwrap();
        assert!(autostart_intent(&path));
    }

    #[test]
    fn settings_round_trip_through_the_file_and_reload_only_on_a_real_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut file = SettingsFile::load(&path);
        assert!(path.exists(), "a missing file is written with defaults");
        assert!(file.reload_if_changed().is_none());

        let mut changed = file.get().clone();
        changed.max_connections = 7;
        file.set(changed);
        assert_eq!(SettingsFile::load(&path).get().max_connections, 7);

        // Touching the file without changing its contents must not notify anyone.
        let text = std::fs::read_to_string(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, text).unwrap();
        assert!(file.reload_if_changed().is_none());
    }
}
