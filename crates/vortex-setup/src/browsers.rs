//! Where each browser looks for native-messaging manifests.
//!
//! Every path here is **per-user**. There are per-machine equivalents (`HKLM`,
//! `/etc/opt/chrome`), and Vortex deliberately does not use them: per-user install with no
//! admin rights is a stated constraint (01 §Process model), and a registration written
//! under `HKLM` by one user applies to every account on the machine.

use crate::Flavour;

/// One browser, and the three places it might keep manifests.
///
/// `None` means the browser does not exist on that platform, or does not read a per-user
/// directory there. A browser in this table with three `None`s is a mistake, and a test
/// says so.
#[derive(Debug, Clone, Copy)]
pub struct Browser {
    pub id: &'static str,
    pub name: &'static str,
    pub flavour: Flavour,
    /// Under `HKCU\Software\`, with `\NativeMessagingHosts\<host name>` appended.
    pub windows_key: Option<&'static str>,
    /// Relative to `$HOME`. Not uniformly under `.config`: Firefox is the exception, and
    /// it is the exception on macOS too.
    pub linux_dir: Option<&'static str>,
    /// Relative to `$HOME`.
    pub macos_dir: Option<&'static str>,
}

/// The six from 06 §Phase 6, plus Chromium — which is one row, is the default browser on
/// several distributions, and would otherwise be a bug report.
///
/// **Opera on Windows** reads Chrome's key rather than its own, which is why the Chrome
/// row covers it there and the Opera row is `None`. Registering a key Opera does not read
/// would look like support and provide none.
pub const BROWSERS: &[Browser] = &[
    Browser {
        id: "chrome",
        name: "Google Chrome",
        flavour: Flavour::Chromium,
        windows_key: Some(r"Google\Chrome"),
        linux_dir: Some(".config/google-chrome/NativeMessagingHosts"),
        macos_dir: Some("Library/Application Support/Google/Chrome/NativeMessagingHosts"),
    },
    Browser {
        id: "chromium",
        name: "Chromium",
        flavour: Flavour::Chromium,
        windows_key: Some("Chromium"),
        linux_dir: Some(".config/chromium/NativeMessagingHosts"),
        macos_dir: Some("Library/Application Support/Chromium/NativeMessagingHosts"),
    },
    Browser {
        id: "edge",
        name: "Microsoft Edge",
        flavour: Flavour::Chromium,
        windows_key: Some(r"Microsoft\Edge"),
        linux_dir: Some(".config/microsoft-edge/NativeMessagingHosts"),
        macos_dir: Some("Library/Application Support/Microsoft Edge/NativeMessagingHosts"),
    },
    Browser {
        id: "brave",
        name: "Brave",
        flavour: Flavour::Chromium,
        windows_key: Some(r"BraveSoftware\Brave-Browser"),
        linux_dir: Some(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        macos_dir: Some(
            "Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        ),
    },
    Browser {
        id: "vivaldi",
        name: "Vivaldi",
        flavour: Flavour::Chromium,
        windows_key: Some("Vivaldi"),
        linux_dir: Some(".config/vivaldi/NativeMessagingHosts"),
        macos_dir: Some("Library/Application Support/Vivaldi/NativeMessagingHosts"),
    },
    Browser {
        id: "opera",
        name: "Opera",
        flavour: Flavour::Chromium,
        windows_key: None,
        linux_dir: Some(".config/opera/NativeMessagingHosts"),
        macos_dir: Some("Library/Application Support/com.operasoftware.Opera/NativeMessagingHosts"),
    },
    Browser {
        // One row for Firefox and its forks: they share `HKCU\Software\Mozilla` on
        // Windows, and `~/.mozilla` on Linux.
        id: "firefox",
        name: "Firefox",
        flavour: Flavour::Gecko,
        windows_key: Some("Mozilla"),
        linux_dir: Some(".mozilla/native-messaging-hosts"),
        macos_dir: Some("Library/Application Support/Mozilla/NativeMessagingHosts"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_registration_path_is_absolute_or_escapes_the_home_directory() {
        // These are joined onto `$HOME`. An absolute component would silently replace it
        // and write outside the user's own space; a `..` would climb out of it.
        for browser in BROWSERS {
            for dir in [browser.linux_dir, browser.macos_dir].into_iter().flatten() {
                let path = std::path::Path::new(dir);
                assert!(path.is_relative(), "{}: {dir}", browser.id);
                assert!(
                    path.components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                    "{}: {dir}",
                    browser.id
                );
            }
        }
    }

    #[test]
    fn every_chromium_browser_reads_a_chromium_shaped_directory() {
        // The two flavours differ only in one manifest key, so the thing that can go
        // wrong is a row with the wrong flavour — a Gecko manifest in a Chromium
        // directory is accepted and matches nothing.
        for browser in BROWSERS {
            let gecko = browser.id == "firefox";
            assert_eq!(browser.flavour == Flavour::Gecko, gecko, "{}", browser.id);
        }
    }
}
