//! Per-user OS integration — the two places Vortex has to write outside its own data
//! directory for anything to work at all.
//!
//! - **Native-messaging registration.** A browser will not start `vortex-host` unless it
//!   finds a manifest naming it. Without one the extension goes passive and *says nothing
//!   about it*, by design (03 §2), so a broken registration is a silent product.
//! - **The login entry.** `vortexd` is a user-session process, not a service (01 §Process
//!   model), so "start when I sign in" is a per-user registry value, a LaunchAgent, or an
//!   XDG autostart file — all three user-scoped and user-removable.
//!
//! ## Why this is a crate and not an installer script
//!
//! The obvious home for this is a WiX custom action. That would be wrong twice over.
//!
//! Registration is not a one-shot install step, it is a **reconciliation**. A user who
//! installs Brave a month after Vortex has a browser Vortex cannot see, and the symptom —
//! capture quietly does nothing — is indistinguishable from a crashed daemon. So `vortexd`
//! re-runs this on every start. It is a handful of registry writes and it is idempotent,
//! which is the whole reason it can be run that often.
//!
//! And an installer custom action is only reachable by running the installer, which means
//! it cannot be tested. Everything here is a pure function over a [`Registration`] plus one
//! thin platform write, and the pure part has tests.

use std::path::{Path, PathBuf};

mod autostart;
mod browsers;
mod manifest;

pub use autostart::{autostart_enabled, set_autostart};
pub use browsers::{Browser, BROWSERS};
pub use manifest::HostManifest;

/// Which of the two manifest shapes a browser reads. The only difference is the key
/// naming who may connect, and putting the wrong one in a browser's directory is accepted
/// silently and matches nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    Chromium,
    Gecko,
}

/// The native-messaging host name. The extension asks for this string by name
/// (`apps/extension/src/build.ts`), so the two must agree exactly.
pub const HOST_NAME: &str = "io.vortex.host";

/// What the extension is allowed to be, on the Chromium side.
///
/// The platform forbids wildcards in `allowed_origins`, so these are exact ids and they
/// are part of the build.
///
/// The one below is the id of every build made from this repository, store package aside.
/// It is not a name anyone chose: Chromium derives an extension's id from its public key,
/// and `apps/extension/identity.ts` declares that key precisely so the answer is the same
/// on every machine instead of a hash of whichever folder the unpacked build happened to
/// be loaded from. `apps/extension/tests/identity.test.ts` fails if the two drift.
///
/// The store listing will have an id of its own, assigned when the item is created; it is
/// a second entry here rather than a replacement, because a developer's build and a
/// user's install both have to reach the same daemon.
///
/// `VORTEX_EXTENSION_ID` (comma-separated) adds ids at run time without a rebuild — for a
/// build made somewhere else, or a store id being tried before it is committed here.
pub const CHROMIUM_IDS: &[&str] = &[
    // The development build, from the key in `apps/extension/identity.ts`.
    "lbapnpokpngacdfbhhmgjckoilhfkjia",
    // The Chrome Web Store listing, assigned when the item was created. A store build
    // ships no key, so this is the id every user who installs from the listing will
    // have — and until it is here, `vortexd --register` writes a host manifest whose
    // `allowed_origins` does not name them and the connection is refused. The symptom
    // is the quiet one the popup now reports as "not installed".
    "nififgnkdkmlnnnfiillfdakkklofgol",
];

/// What the extension is on the Gecko side. Fixed by `browser_specific_settings.gecko.id`
/// in `apps/extension/wxt.config.ts`, so unlike the Chromium ids it is knowable now.
pub const GECKO_ID: &str = "vortex@vortex.download";

/// Everything a registration points at. Built once and passed to [`register`].
#[derive(Debug, Clone)]
pub struct Registration {
    /// Absolute path to `vortex-host` — the program the browser will actually start.
    pub host: PathBuf,
    /// Absolute path to `vortexd`, for the login entry.
    pub daemon: PathBuf,
    /// Where generated manifests are kept on Windows, whose registration is a registry
    /// value holding a path. Unix has no such indirection: the manifest goes in the
    /// browser's own directory, and this is unused there.
    pub manifest_dir: PathBuf,
    /// Exact Chromium extension ids. Empty means the Chromium browsers cannot be
    /// registered usefully, which [`register`] reports rather than papering over.
    pub chromium_ids: Vec<String>,
    /// What the per-browser locations in [`BROWSERS`] are relative to: the `HKCU` subtree
    /// on Windows, the home directory elsewhere.
    ///
    /// A real install never changes this. A test does, so that running the suite cannot
    /// overwrite a browser registration the developer is actually using — which is the
    /// difference between this crate being testable and being taken on faith.
    pub root: PathBuf,
}

impl Registration {
    /// The installed layout: `vortex-host` and `vortexd` sit beside whoever is asking,
    /// and generated manifests live under the per-user data directory.
    ///
    /// Beside-the-caller rather than `PATH` for the same reason `vortex_ipc::start` does
    /// it: `PATH` is a way for something else to be started instead, and a client and a
    /// daemon that were installed together are always in one directory.
    ///
    /// The manifests deliberately do **not** go in the install directory. A per-machine
    /// MSI puts that under `Program Files`, where a user-session daemon cannot write, and
    /// self-healing on every start is the point.
    pub fn new(data_dir: &Path) -> std::io::Result<Self> {
        let dir = std::env::current_exe()?
            .parent()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "the caller is not in a directory")
            })?
            .to_path_buf();
        Ok(Self {
            host: dir.join(exe("vortex-host")),
            daemon: dir.join(exe("vortexd")),
            manifest_dir: data_dir.join("native-messaging"),
            chromium_ids: chromium_ids(),
            root: default_root()?,
        })
    }
}

#[cfg(windows)]
fn default_root() -> std::io::Result<PathBuf> {
    Ok(PathBuf::from("Software"))
}

#[cfg(not(windows))]
fn default_root() -> std::io::Result<PathBuf> {
    dirs::home_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))
}

fn exe(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// The compiled-in ids plus anything in `VORTEX_EXTENSION_ID`, ignoring malformed entries.
///
/// A malformed id is worse than a missing one: the browser accepts the manifest, matches
/// nothing, and starts no host — the same silence as no registration at all, with a file
/// on disk that says otherwise.
fn chromium_ids() -> Vec<String> {
    let mut ids: Vec<String> = CHROMIUM_IDS.iter().map(|id| (*id).to_owned()).collect();
    if let Ok(extra) = std::env::var("VORTEX_EXTENSION_ID") {
        for id in extra.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            if is_chromium_id(id) {
                ids.push(id.to_owned());
            } else {
                tracing::warn!("VORTEX_EXTENSION_ID contains {id:?}, which is not an extension id");
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

/// Chromium ids are 32 characters from `a`–`p`: the first 16 bytes of the SHA-256 of the
/// public key, with each nibble mapped into that alphabet.
pub fn is_chromium_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// What happened to one browser. Every variant is a thing an operator might need to act
/// on, which is why "already correct" is not folded into "written".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// The registration was missing or wrong, and now is not.
    Written,
    /// Already exactly right. The common case on every start after the first.
    Unchanged,
    /// This browser keeps its manifests in its own directory and that directory is not
    /// there, so the browser is not installed for this user. Nothing to do until it is —
    /// and `vortexd` reconciles on every start, so installing it later is enough.
    Absent,
    Removed,
    /// This platform has no separate registration location for this browser — it reads
    /// another's. Distinct from [`State::Absent`], because reporting an installed Opera
    /// as "not installed" is a lie that costs a support ticket.
    Covered,
    /// Registered, but with no identity the browser will match — see [`CHROMIUM_IDS`].
    /// Written anyway, because the failure to fix is a build-time one and a half-written
    /// registration is easier to diagnose than an absent one.
    NoIdentity,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub browser: &'static str,
    pub state: State,
}

/// Writes the manifest for every browser that can take one. Idempotent.
///
/// Errors are per-browser and never fatal: one browser with a permission problem must not
/// cost the user the other five.
pub fn register(reg: &Registration) -> Vec<Outcome> {
    BROWSERS
        .iter()
        .map(|browser| Outcome {
            browser: browser.id,
            state: match platform::write(browser, &manifest::of(browser, reg), reg) {
                Err(e) => State::Failed(e.to_string()),
                // A Chromium registration with no ids in it is written and then reported
                // as what it is. Silently succeeding is how this ends up shipped.
                Ok(State::Written | State::Unchanged)
                    if browser.flavour == Flavour::Chromium && reg.chromium_ids.is_empty() =>
                {
                    State::NoIdentity
                }
                Ok(state) => state,
            },
        })
        .collect()
}

/// Removes every registration this crate can write. Used by the uninstaller.
///
/// Uninstalling must not leave a registry value pointing at a deleted executable: a
/// browser that finds one starts a host that is not there, and reports it to the user as
/// the extension being broken.
pub fn unregister(reg: &Registration) -> Vec<Outcome> {
    BROWSERS
        .iter()
        .map(|browser| Outcome {
            browser: browser.id,
            state: match platform::remove(browser, reg) {
                Ok(state) => state,
                Err(e) => State::Failed(e.to_string()),
            },
        })
        .collect()
}

/// One line per browser, for `vortexd --register` and for the log.
pub fn summarise(outcomes: &[Outcome]) -> String {
    let mut out = String::new();
    for outcome in outcomes {
        let state = match &outcome.state {
            State::Written => "registered".to_owned(),
            State::Unchanged => "already registered".to_owned(),
            State::Absent => "not installed".to_owned(),
            State::Removed => "removed".to_owned(),
            State::Covered => "registered through another browser on this platform".to_owned(),
            State::NoIdentity => "registered, but no extension id is pinned".to_owned(),
            State::Failed(e) => format!("FAILED: {e}"),
        };
        out.push_str(&format!("{:<12} {state}\n", outcome.browser));
    }
    out
}

#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(not(windows), path = "unix.rs")]
mod platform;

#[cfg(test)]
mod tests {
    use super::*;

    /// A registration pointing at nothing real. Only `root` and `manifest_dir` are used by
    /// the platform writers; the executables never have to exist, because registering is
    /// writing down a path and not resolving one.
    pub(crate) fn sample(root: &Path) -> Registration {
        Registration {
            host: PathBuf::from("/nowhere/vortex-host"),
            daemon: PathBuf::from("/nowhere/vortexd"),
            manifest_dir: root.join("manifests"),
            chromium_ids: vec!["a".repeat(32)],
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn an_extension_id_is_thirty_two_letters_from_the_first_sixteen_of_the_alphabet() {
        assert!(is_chromium_id("abcdefghijklmnopabcdefghijklmnop"));
        // Real ids look like this. Anything outside a–p is a typo or a different kind of
        // identifier, and either way the browser will match nothing.
        assert!(!is_chromium_id("abcdefghijklmnopabcdefghijklmnoq"));
        assert!(!is_chromium_id("ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP"));
        assert!(!is_chromium_id("tooshort"));
        assert!(!is_chromium_id(""));
    }

    #[test]
    fn every_compiled_in_id_is_one_a_browser_could_match() {
        // A malformed id is worse than a missing one: the browser accepts the manifest,
        // matches nothing, and starts no host — so capture goes quiet with no error
        // anywhere. `chromium_ids` rejects a malformed `VORTEX_EXTENSION_ID` at run time;
        // this is the same guarantee for the ones that ship.
        for id in CHROMIUM_IDS {
            assert!(is_chromium_id(id), "{id} is not an extension id");
        }
        assert!(!CHROMIUM_IDS.is_empty(), "the Chromium side would register nothing");
    }

    #[test]
    fn every_browser_in_the_table_can_be_registered_on_at_least_one_platform() {
        for browser in BROWSERS {
            assert!(
                browser.windows_key.is_some()
                    || browser.linux_dir.is_some()
                    || browser.macos_dir.is_some(),
                "{} is in the table but has nowhere to go",
                browser.id
            );
        }
    }

    #[test]
    fn browser_ids_are_unique() {
        let mut ids: Vec<_> = BROWSERS.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    /// The whole cycle against a real registry / a real filesystem, under a root of its
    /// own so that a test run cannot touch a browser registration the developer uses.
    ///
    /// The three assertions are the three promises: it works, doing it twice changes
    /// nothing, and uninstalling leaves nothing pointing at a deleted executable.
    #[test]
    fn registering_is_idempotent_and_unregistering_undoes_it() {
        let scratch = tempfile::tempdir().unwrap();
        let root = arrange(&scratch);
        let reg = sample(&root);

        let first = register(&reg);
        assert!(
            first.iter().any(|o| o.state == State::Written),
            "nothing was registered: {first:?}"
        );
        assert!(!first.iter().any(|o| matches!(o.state, State::Failed(_))), "{first:?}");

        let again = register(&reg);
        for (before, after) in first.iter().zip(&again) {
            let settled = match &before.state {
                State::Written => State::Unchanged,
                other => other.clone(),
            };
            assert_eq!(after.state, settled, "{} is not idempotent", after.browser);
        }

        let removed = unregister(&reg);
        assert!(removed.iter().any(|o| o.state == State::Removed), "{removed:?}");
        for outcome in unregister(&reg) {
            assert!(
                matches!(outcome.state, State::Absent | State::Covered),
                "{} still has a registration: {:?}",
                outcome.browser,
                outcome.state
            );
        }
        cleanup(&root);
    }

    /// Windows registrations are registry values, so the scratch root is a registry path
    /// rather than a directory; the temporary directory still holds the manifests.
    #[cfg(windows)]
    fn arrange(_scratch: &tempfile::TempDir) -> PathBuf {
        PathBuf::from("Software")
            .join("VortexTest")
            .join(std::process::id().to_string())
    }

    #[cfg(windows)]
    fn cleanup(root: &Path) {
        let _ = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .delete_subkey_all(root);
    }

    /// Unix writes into the browser's own directory and only when the browser is there, so
    /// the arrangement is a fake home with two of the seven actually installed.
    #[cfg(not(windows))]
    fn arrange(scratch: &tempfile::TempDir) -> PathBuf {
        let home = scratch.path().to_path_buf();
        for browser in ["chrome", "firefox"] {
            let entry = BROWSERS.iter().find(|b| b.id == browser).unwrap();
            let relative = if cfg!(target_os = "macos") { entry.macos_dir } else { entry.linux_dir };
            let dir = home.join(relative.unwrap());
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        }
        home
    }

    #[cfg(not(windows))]
    fn cleanup(_root: &Path) {}

    /// The unix writer must not create a configuration directory for a browser the user
    /// does not have — `~/.config` is the user's, and littering it with five browsers
    /// they never installed is the kind of thing that gets an app uninstalled.
    #[cfg(not(windows))]
    #[test]
    fn a_browser_that_is_not_installed_is_left_alone_entirely() {
        let scratch = tempfile::tempdir().unwrap();
        let root = arrange(&scratch);
        for outcome in register(&sample(&root)) {
            if matches!(outcome.browser, "chrome" | "firefox") {
                assert_eq!(outcome.state, State::Written, "{}", outcome.browser);
            } else {
                assert_eq!(outcome.state, State::Absent, "{}", outcome.browser);
            }
        }
        let entry = BROWSERS.iter().find(|b| b.id == "vivaldi").unwrap();
        let relative = if cfg!(target_os = "macos") { entry.macos_dir } else { entry.linux_dir };
        assert!(!root.join(relative.unwrap()).exists());
    }
}
