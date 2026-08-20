//! macOS and Linux registration: the manifest *is* the registration. It goes in the
//! browser's own directory, named after the host, and there is no registry indirection.
//!
//! Which means, unlike Windows, writing for a browser that is not installed creates a
//! directory tree under `~/.config` for software the user does not have. So here the
//! browser's own configuration directory has to already exist — and because `vortexd`
//! reconciles on every start, installing the browser later still works, one restart on.

use std::io;
use std::path::{Path, PathBuf};

use crate::{manifest, Browser, HostManifest, Registration, State, HOST_NAME};

/// `None` when this browser has no per-user location on this platform.
fn dir(browser: &Browser, reg: &Registration) -> Option<PathBuf> {
    let relative = if cfg!(target_os = "macos") {
        browser.macos_dir?
    } else {
        browser.linux_dir?
    };
    Some(reg.root.join(relative))
}

/// A browser is considered present when the directory *containing* its native-messaging
/// directory exists — `~/.config/google-chrome`, `~/.mozilla`. The manifest directory
/// itself routinely does not exist until something creates it, so testing for that would
/// report every fresh profile as no browser at all.
fn present(dir: &Path) -> bool {
    dir.parent().is_some_and(Path::exists)
}

pub fn write(browser: &Browser, body: &HostManifest, reg: &Registration) -> io::Result<State> {
    let Some(dir) = dir(browser, reg) else {
        return Ok(State::Covered);
    };
    if !present(&dir) {
        return Ok(State::Absent);
    }
    manifest::write_if_changed(&dir.join(format!("{HOST_NAME}.json")), &body.to_json())
}

pub fn remove(browser: &Browser, reg: &Registration) -> io::Result<State> {
    let Some(dir) = dir(browser, reg) else {
        return Ok(State::Covered);
    };
    match std::fs::remove_file(dir.join(format!("{HOST_NAME}.json"))) {
        Ok(()) => Ok(State::Removed),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::Absent),
        Err(e) => Err(e),
    }
}

// ── The login entry ─────────────────────────────────────────────────────────

fn autostart_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents/io.vortex.daemon.plist")
    } else {
        home.join(".config/autostart/vortex.desktop")
    })
}

/// `RunAtLoad` and nothing else: no `KeepAlive`, because a program `launchd` restarts on
/// every exit cannot be quit, and quitting Vortex is something a user is allowed to do.
fn macos_plist(program: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.vortex.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        xml_escape(&program.to_string_lossy()),
        crate::autostart::TRAY_FLAG
    )
}

/// `NoDisplay` because a startup list is for things the user might want to launch, and this
/// entry launches Vortex into the tray with no window — the menu entry for opening it is
/// the application's own, installed by the package.
fn xdg_desktop(program: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Vortex\n\
         Comment=The Vortex download manager\n\
         Exec=\"{}\" {}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        program.display(),
        crate::autostart::TRAY_FLAG
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn set_autostart(enabled: bool, program: &Path) -> io::Result<()> {
    let path = autostart_path()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    let body = if cfg!(target_os = "macos") {
        macos_plist(program)
    } else {
        xdg_desktop(program)
    };
    manifest::write_if_changed(&path, &body).map(|_| ())
}

pub fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|path| path.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_xml_metacharacters_cannot_break_out_of_the_plist() {
        // A home directory may contain `&`. A plist that is not well-formed is not
        // ignored by `launchd`, it is a startup that silently never happens.
        let plist = macos_plist(Path::new("/Users/a&b/Vortex.app/Contents/MacOS/vortex-app"));
        assert!(plist.contains("<string>/Users/a&amp;b/"), "{plist}");
        assert!(plist.contains("<string>--tray</string>"), "{plist}");
    }

    #[test]
    fn the_desktop_entry_quotes_the_path_and_stays_out_of_the_menu() {
        let entry = xdg_desktop(Path::new("/home/a b/vortex-app"));
        assert!(entry.contains("Exec=\"/home/a b/vortex-app\" --tray"), "{entry}");
        assert!(entry.contains("NoDisplay=true"));
    }
}
