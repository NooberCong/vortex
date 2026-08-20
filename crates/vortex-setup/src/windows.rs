//! Windows registration: a `HKCU` value whose default data is the path of a manifest file.
//!
//! The indirection is the platform's, not ours — the browser reads the registry to find
//! the file, then reads the file to find the executable. Two things follow.
//!
//! The manifest files go under the **data** directory rather than the install directory.
//! A per-machine MSI puts the install directory under `Program Files`, which a
//! user-session daemon cannot write to, and self-healing on every start is the whole point
//! of this module existing rather than an installer custom action.
//!
//! And there is no "is this browser installed" check. Writing a value under
//! `HKCU\Software\Vivaldi` when Vivaldi is not installed costs nothing, is invisible, and
//! means that installing Vivaldi tomorrow works immediately. Detection would be a
//! heuristic that can only produce false negatives, in exchange for nothing.

use std::io;
use std::path::{Path, PathBuf};

use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

use crate::{manifest, Browser, HostManifest, Registration, State, HOST_NAME};

pub fn write(browser: &Browser, body: &HostManifest, reg: &Registration) -> io::Result<State> {
    let Some(key) = browser.windows_key else {
        return Ok(State::Covered);
    };
    let path = reg.manifest_dir.join(manifest::file_name(browser.flavour));
    let file = manifest::write_if_changed(&path, &body.to_json())?;
    let value = point_at(&host_key(reg, key), &path)?;
    Ok(match (file, value) {
        (State::Unchanged, State::Unchanged) => State::Unchanged,
        _ => State::Written,
    })
}

pub fn remove(browser: &Browser, reg: &Registration) -> io::Result<State> {
    let Some(key) = browser.windows_key else {
        return Ok(State::Covered);
    };
    // The file is shared by every browser of the same flavour, so it is removed once and
    // the rest of the passes find it gone. Missing is the desired end state either way.
    let path = reg.manifest_dir.join(manifest::file_name(browser.flavour));
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != io::ErrorKind::NotFound {
            return Err(e);
        }
    }

    let hosts = reg.root.join(key).join("NativeMessagingHosts");
    let parent = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(hosts, KEY_READ | KEY_WRITE);
    match parent {
        // A plain delete rather than a recursive one: our key holds nothing but its
        // default value, and recursing through a key the *browser* owns would be a good
        // way to take its other native messaging hosts with us.
        Ok(parent) => match parent.delete_subkey(HOST_NAME) {
            Ok(()) => Ok(State::Removed),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::Absent),
            Err(e) => Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::Absent),
        Err(e) => Err(e),
    }
}

/// `HKCU\<root>\<browser>\NativeMessagingHosts\io.vortex.host`.
fn host_key(reg: &Registration, browser_key: &str) -> PathBuf {
    reg.root
        .join(browser_key)
        .join("NativeMessagingHosts")
        .join(HOST_NAME)
}

/// Points one browser's key at the manifest, writing only if it does not already say so.
fn point_at(key: &Path, manifest: &Path) -> io::Result<State> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let wanted = manifest.to_string_lossy().into_owned();

    // The default value — the unnamed one, which is what the platform reads.
    if let Ok(existing) = hkcu.open_subkey(key) {
        if existing.get_value::<String, _>("").is_ok_and(|v| v == wanted) {
            return Ok(State::Unchanged);
        }
    }
    let (created, _) = hkcu.create_subkey(key)?;
    created.set_value("", &wanted)?;
    Ok(State::Written)
}

/// `HKCU\...\Run` — user-scoped, user-removable, and visible in Task Manager's Startup
/// tab, which is where a user who wants it gone will look first (01 §Process model).
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Vortex";

/// Quoted, because the path contains spaces on any normal install and an unquoted value is
/// parsed by `CreateProcess`'s ambiguous rules — which will happily start `C:\Program.exe`
/// instead, if someone has arranged for one to exist. The flag sits outside the quotes,
/// where an argument goes.
fn run_command(program: &Path) -> String {
    format!("\"{}\" {}", program.display(), crate::autostart::TRAY_FLAG)
}

pub fn set_autostart(enabled: bool, program: &Path) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu.create_subkey(RUN_KEY)?;
    if !enabled {
        return match run.delete_value(RUN_VALUE) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
    }
    let wanted = run_command(program);
    if run.get_value::<String, _>(RUN_VALUE).is_ok_and(|v| v == wanted) {
        return Ok(());
    }
    run.set_value(RUN_VALUE, &wanted)
}

pub fn autostart_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|run| run.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_command_is_quoted_and_asks_for_the_tray() {
        let command = run_command(Path::new(r"C:\Program Files\Vortex\vortex-app.exe"));
        assert_eq!(command, r#""C:\Program Files\Vortex\vortex-app.exe" --tray"#);
    }

    #[test]
    fn the_host_key_is_where_the_platform_documents_it() {
        let reg = crate::tests::sample(Path::new("Software"));
        assert_eq!(
            host_key(&reg, r"Google\Chrome"),
            PathBuf::from(r"Software\Google\Chrome\NativeMessagingHosts\io.vortex.host")
        );
    }
}
