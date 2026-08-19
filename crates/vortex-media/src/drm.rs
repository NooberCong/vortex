//! The DRM boundary (04 §The DRM boundary, 03 §Denylist).
//!
//! Vortex does not download DRM-protected video. Not with a flag, not in a developer mode,
//! not behind an advanced setting. This module is where that stops being a policy document
//! and starts being code: a manifest is checked *before* a single segment is fetched, and
//! a protected one is refused with a sentence that says so plainly.
//!
//! Clear-key AES-128 is not DRM. It is a key served over HTTP with the same session
//! credentials as the manifest, and it is squarely in scope.

use crate::hls::Method;

/// Why a stream will not be downloaded. This is a refusal, not a failure — nothing about
/// it is worth retrying, and the user is told the reason rather than shown an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// "Widevine", "FairPlay", … or a bare scheme when the system is unrecognised.
    pub system: String,
    pub message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

impl Refusal {
    fn new(system: impl Into<String>) -> Self {
        let system = system.into();
        Self {
            message: format!("This video is protected by {system}. Vortex doesn't download DRM."),
            system,
        }
    }
}

/// Something Vortex could legally download but this build cannot yet decrypt. Kept apart
/// from [`Refusal`] on purpose: "we won't" and "we can't" are different sentences, and
/// conflating them would make the DRM boundary look like a missing feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub message: String,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Unsupported {}

/// Known key systems, by the identifiers that appear in manifests. The UUIDs are the
/// registered DASH `schemeIdUri` values; the reverse-DNS names are HLS `KEYFORMAT`s.
const SYSTEMS: &[(&str, &str)] = &[
    ("edef8ba9-79d6-4ace-a3c8-27dcd51d21ed", "Widevine"),
    ("9a04f079-9840-4286-ab92-e65be0885f95", "PlayReady"),
    ("94ce86fb-07ff-4f43-adb8-93d2fa968ca2", "FairPlay"),
    ("f239e769-efa3-4850-9c16-a903c6932efb", "Adobe Primetime"),
    ("com.widevine.alpha", "Widevine"),
    ("com.microsoft.playready", "PlayReady"),
    ("com.apple.streamingkeydelivery", "FairPlay"),
];

/// `KEYFORMAT` values that mean "a key you fetch over HTTP", i.e. not DRM.
const CLEAR_KEY_FORMATS: &[&str] = &["identity", "urn:mpeg:dash:sea:2012"];

pub fn name_for(identifier: &str) -> Option<&'static str> {
    let needle = identifier.trim().trim_start_matches("urn:uuid:").to_ascii_lowercase();
    SYSTEMS
        .iter()
        .find(|(id, _)| needle.contains(id))
        .map(|(_, name)| *name)
}

/// An `#EXT-X-KEY`. AES-128 with a clear key is fine; anything with a DRM key format is
/// refused whatever its method says.
pub fn check_hls_key(method: Method, key_format: Option<&str>) -> Result<(), Refusal> {
    if let Some(format) = key_format {
        let format = format.trim();
        if !format.is_empty()
            && !CLEAR_KEY_FORMATS
                .iter()
                .any(|clear| format.eq_ignore_ascii_case(clear))
        {
            return Err(Refusal::new(
                name_for(format).unwrap_or("a content protection system"),
            ));
        }
    }
    // A FairPlay stream that omits KEYFORMAT still announces itself by method.
    if method == Method::SampleAes && key_format.is_none() {
        return Err(Refusal::new("FairPlay"));
    }
    Ok(())
}

/// A DASH `ContentProtection@schemeIdUri`. Unlike HLS there is no widely used clear-key
/// convention, so any protection element is a refusal.
pub fn check_dash_scheme(scheme: &str) -> Result<(), Refusal> {
    // `mp4protection` only declares *that* the content is Common Encryption; the system
    // that holds the key is declared alongside it. Either way it is encrypted.
    let named = name_for(scheme).unwrap_or_else(|| {
        if scheme.contains("mp4protection") {
            "Common Encryption"
        } else {
            "a content protection system"
        }
    });
    Err(Refusal::new(named))
}

/// Encryption this build could legally handle but has not implemented. Only clear-key
/// `SAMPLE-AES` reaches here: it needs per-sample NAL parsing rather than a whole-segment
/// CBC pass, and shipping a half-implementation would produce files that look downloaded
/// and play as noise.
pub fn check_supported(method: Method) -> Result<(), Unsupported> {
    if method == Method::SampleAes {
        return Err(Unsupported {
            message: "This stream uses sample-level encryption, which Vortex can't decrypt yet."
                .to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clear_key_is_not_drm() {
        assert!(check_hls_key(Method::Aes128, Some("identity")).is_ok());
        assert!(check_hls_key(Method::Aes128, None).is_ok());
    }

    #[test]
    fn fairplay_is_refused_by_name() {
        let refusal = check_hls_key(Method::SampleAes, Some("com.apple.streamingkeydelivery"))
            .unwrap_err();
        assert_eq!(refusal.system, "FairPlay");
        assert!(refusal.message.contains("doesn't download DRM"), "{refusal}");
    }

    #[test]
    fn widevine_is_refused_however_it_spells_itself() {
        assert_eq!(
            check_dash_scheme("urn:uuid:EDEF8BA9-79D6-4ACE-A3C8-27DCD51D21ED")
                .unwrap_err()
                .system,
            "Widevine"
        );
        assert_eq!(
            check_hls_key(Method::Aes128, Some("com.widevine.alpha"))
                .unwrap_err()
                .system,
            "Widevine"
        );
    }

    #[test]
    fn an_unknown_protection_scheme_is_still_refused() {
        let refusal = check_dash_scheme("urn:uuid:00000000-0000-0000-0000-000000000000").unwrap_err();
        assert_eq!(refusal.system, "a content protection system");
    }

    #[test]
    fn sample_aes_with_a_clear_key_is_declined_as_unsupported_not_as_drm() {
        // The distinction the user reads: "Vortex can't" rather than "Vortex won't".
        assert!(check_hls_key(Method::SampleAes, Some("identity")).is_ok());
        let e = check_supported(Method::SampleAes).unwrap_err();
        assert!(e.message.contains("can't decrypt yet"), "{e}");
        assert!(check_supported(Method::Aes128).is_ok());
    }
}
