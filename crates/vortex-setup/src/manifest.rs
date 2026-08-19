//! The manifest itself — the file that tells a browser `vortex-host` exists and who may
//! talk to it.
//!
//! Two shapes, one key apart. Chromium matches an *origin* (`chrome-extension://<id>/`),
//! Gecko matches an *extension id*. Both forbid wildcards, and both fail closed and
//! silently: a manifest listing an id that is not the id of the installed build produces
//! a browser that starts no host and an extension that goes passive.

use serde::Serialize;

use crate::{Browser, Flavour, Registration, HOST_NAME};

/// Field order matters here only because `apps/extension/native-messaging/*.json` document
/// this shape and a test compares the two. Serde emits struct fields in declaration order.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct HostManifest {
    pub name: &'static str,
    pub description: &'static str,
    /// Absolute, always. The platform rejects a relative path on Windows and resolves it
    /// against an unspecified directory elsewhere.
    pub path: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_origins: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_extensions: Option<Vec<String>>,
}

const DESCRIPTION: &str = "Vortex download manager - bridges the extension to the local daemon.";

pub fn of(browser: &Browser, reg: &Registration) -> HostManifest {
    let (origins, extensions) = match browser.flavour {
        Flavour::Chromium => (
            Some(
                reg.chromium_ids
                    .iter()
                    .map(|id| format!("chrome-extension://{id}/"))
                    .collect(),
            ),
            None,
        ),
        Flavour::Gecko => (None, Some(vec![crate::GECKO_ID.to_owned()])),
    };
    HostManifest {
        name: HOST_NAME,
        description: DESCRIPTION,
        path: reg.host.to_string_lossy().into_owned(),
        kind: "stdio",
        allowed_origins: origins,
        allowed_extensions: extensions,
    }
}

impl HostManifest {
    /// Pretty, because a user debugging a silent extension will open this file, and
    /// trailing newline because every other file this product writes has one.
    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("a manifest is always valid JSON");
        json.push('\n');
        json
    }
}

/// Writes only when the contents differ, and reports which it was.
///
/// The difference is not cosmetic. `vortexd` reconciles on every start, and rewriting an
/// identical file every time would touch its mtime — which is exactly what a browser
/// watching its own manifest directory, or a backup tool, reacts to. It also turns the
/// per-start log line into noise, and a log that always says "wrote" cannot be used to
/// notice that something keeps un-writing it.
pub fn write_if_changed(path: &std::path::Path, contents: &str) -> std::io::Result<crate::State> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(crate::State::Unchanged);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(crate::State::Written)
}

/// The file name a flavour's manifest gets on Windows, where the registration is a
/// registry value holding a path rather than the file's location itself.
///
/// One file per flavour rather than per browser: the contents depend only on the flavour,
/// and six identical files is six chances for five of them to go stale.
pub fn file_name(flavour: Flavour) -> &'static str {
    match flavour {
        Flavour::Chromium => "chromium.json",
        Flavour::Gecko => "gecko.json",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn placeholder(ids: &[&str]) -> Registration {
        Registration {
            host: PathBuf::from(r"%INSTALL_DIR%\vortex-host.exe"),
            daemon: PathBuf::from(r"%INSTALL_DIR%\vortexd.exe"),
            manifest_dir: PathBuf::from("."),
            chromium_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            root: PathBuf::from("."),
        }
    }

    fn browser(id: &str) -> &'static Browser {
        crate::BROWSERS.iter().find(|b| b.id == id).unwrap()
    }

    /// `apps/extension/native-messaging/*.json` exist so that someone reading the
    /// extension can see what the installer writes. That is only true while they match
    /// what it writes, and a documentation file nobody checks is a documentation file
    /// that lies. Rendering with the placeholders those files use is the check.
    #[test]
    fn the_checked_in_templates_still_describe_what_this_generates() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/extension/native-messaging");
        for (id, file) in [("chrome", "chrome.json"), ("firefox", "firefox.json")] {
            let generated = of(browser(id), &placeholder(&["%EXTENSION_ID%"])).to_json();
            let checked_in = std::fs::read_to_string(root.join(file)).unwrap();
            assert_eq!(
                generated.replace("\r\n", "\n"),
                checked_in.replace("\r\n", "\n"),
                "{file} no longer matches the manifest this crate writes"
            );
        }
    }

    #[test]
    fn a_chromium_manifest_carries_origins_and_a_gecko_one_carries_ids() {
        let chromium = of(browser("chrome"), &placeholder(&["a".repeat(32).as_str()]));
        assert_eq!(
            chromium.allowed_origins.as_deref(),
            Some(&[format!("chrome-extension://{}/", "a".repeat(32))][..])
        );
        assert!(chromium.allowed_extensions.is_none());

        let gecko = of(browser("firefox"), &placeholder(&[]));
        assert_eq!(gecko.allowed_extensions.as_deref(), Some(&[crate::GECKO_ID.to_owned()][..]));
        assert!(gecko.allowed_origins.is_none());
        // Gecko's identity does not come from the Chromium id list, so an empty list must
        // not disarm Firefox — it is the flagship target (03 §Store strategy).
        assert!(!gecko.to_json().contains("allowed_origins"));
    }

    #[test]
    fn a_rewrite_of_identical_contents_is_reported_as_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("chromium.json");
        assert_eq!(write_if_changed(&path, "one").unwrap(), crate::State::Written);
        assert_eq!(write_if_changed(&path, "one").unwrap(), crate::State::Unchanged);
        assert_eq!(write_if_changed(&path, "two").unwrap(), crate::State::Written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn a_windows_path_survives_json_escaping_intact() {
        let manifest = of(browser("chrome"), &placeholder(&[]));
        let round_trip: serde_json::Value = serde_json::from_str(&manifest.to_json()).unwrap();
        assert_eq!(round_trip["path"], serde_json::json!(r"%INSTALL_DIR%\vortex-host.exe"));
    }
}
