//! The extractor fallback (04 §yt-dlp).
//!
//! ```text
//! Vortex parser succeeds → Vortex scheduler downloads it   (the fast path)
//! Vortex parser fails    → yt-dlp --dump-json              (extract only)
//!                        → Vortex scheduler downloads it   (still the fast path)
//! ```
//!
//! **yt-dlp extracts; it never downloads.** Its own downloader would give up adaptive
//! concurrency, hedging, resume and the writer — every property this project exists for.
//! So it is asked one question, `--dump-single-json`, and its answer is turned into the
//! same [`MediaCandidate`] the first-party parsers produce.
//!
//! Credentials deliberately do not cross this boundary. Headers appear in a child process's
//! argv, and the cookie jar is memory-only by design (01 §Security boundaries), so an
//! extraction runs unauthenticated. A site that needs the session is a site whose manifest
//! our own parser reads — that path carries the whole envelope.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use vortex_proto::{MediaCandidate, MediaKind, MediaTrack, MediaVariant, RequestEnvelope};

/// Headers worth replaying to an extractor. Everything that identifies a *user* is absent.
const FORWARDED: [&str; 2] = ["user-agent", "referer"];

#[derive(Debug, Clone)]
pub struct YtDlp(PathBuf);

impl YtDlp {
    /// Bundled beside the daemon first — extractors break weekly and ship on their own
    /// channel — then `VORTEX_YTDLP`, then `PATH`.
    pub fn find() -> Option<Self> {
        let name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
        if let Some(beside) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
            .filter(|path| path.is_file())
        {
            return Some(Self(beside));
        }
        if let Some(configured) = std::env::var_os("VORTEX_YTDLP").map(PathBuf::from) {
            if configured.is_file() {
                return Some(Self(configured));
            }
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .map(Self)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Asks yt-dlp what is on a page. Never asks it to fetch anything.
pub async fn extract(
    tool: &YtDlp,
    envelope: &RequestEnvelope,
    preferred_height: u32,
) -> Result<MediaCandidate, String> {
    let url = envelope.effective_url();
    let mut command = tokio::process::Command::new(tool.path());
    command
        .arg("--dump-single-json")
        .arg("--no-warnings")
        .arg("--no-playlist")
        .arg("--skip-download");
    for (name, value) in &envelope.headers {
        if FORWARDED.iter().any(|f| name.eq_ignore_ascii_case(f)) {
            command.arg("--add-header").arg(format!("{name}:{value}"));
        }
    }
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = command
        .output()
        .await
        .map_err(|e| format!("yt-dlp would not start: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let last = detail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!("Vortex couldn't find a video on that page. {last}"));
    }
    parse(&String::from_utf8_lossy(&output.stdout), url, preferred_height)
}

/// Turns one `--dump-single-json` object into a candidate. Pure, so the shape of yt-dlp's
/// output can be asserted on without yt-dlp installed.
pub fn parse(json: &str, url: &str, preferred_height: u32) -> Result<MediaCandidate, String> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("yt-dlp returned something unreadable: {e}"))?;

    let title = root
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("video")
        .to_owned();
    let duration = root.get("duration").and_then(|d| d.as_f64());
    let live = root.get("is_live").and_then(|l| l.as_bool()).unwrap_or(false);
    let formats = root
        .get("formats")
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();

    let mut variants: Vec<(u8, MediaVariant)> = Vec::new();
    let mut audio: Vec<MediaTrack> = Vec::new();
    // Bitrates for `audio`, positionally — `MediaTrack` has nowhere to carry one, and
    // choosing a default without it means choosing whichever the extractor listed first.
    let mut audio_bandwidth: Vec<u64> = Vec::new();
    let mut kind = MediaKind::Progressive;

    for format in &formats {
        let Some(media_url) = format.get("url").and_then(|u| u.as_str()) else {
            continue;
        };
        let protocol = format
            .get("protocol")
            .and_then(|p| p.as_str())
            .unwrap_or_default();
        // Anything that is not plain HTTP or an HLS/DASH manifest is a protocol we would
        // have to reimplement, which is exactly what this fallback exists to avoid.
        let manifest = match protocol {
            p if p.contains("m3u8") => Some(MediaKind::Hls),
            p if p.contains("dash") => Some(MediaKind::Dash),
            "https" | "http" => None,
            _ => continue,
        };
        if let Some(manifest) = manifest {
            kind = manifest;
        }

        let vcodec = format.get("vcodec").and_then(|c| c.as_str()).unwrap_or("none");
        let acodec = format.get("acodec").and_then(|c| c.as_str()).unwrap_or("none");
        let bandwidth = format
            .get("tbr")
            .and_then(|b| b.as_f64())
            .map(|kbps| (kbps * 1000.0) as u64)
            .unwrap_or(0);
        let size = format
            .get("filesize")
            .or_else(|| format.get("filesize_approx"))
            .and_then(|s| s.as_u64());

        if vcodec == "none" {
            audio.push(MediaTrack {
                id: media_url.to_owned(),
                label: format
                    .get("format_note")
                    .and_then(|n| n.as_str())
                    .unwrap_or(acodec)
                    .to_owned(),
                language: format
                    .get("language")
                    .and_then(|l| l.as_str())
                    .map(str::to_owned),
                codec_label: (acodec != "none").then(|| acodec.to_owned()),
                estimated_bytes: size,
                default: false,
            });
            audio_bandwidth.push(bandwidth);
            continue;
        }

        variants.push((
            codec_rank(vcodec),
            MediaVariant {
                id: media_url.to_owned(),
                width: format.get("width").and_then(|w| w.as_u64()).map(|w| w as u32),
                height: format.get("height").and_then(|h| h.as_u64()).map(|h| h as u32),
                bandwidth,
                codec_label: codec_label(vcodec, acodec),
                estimated_bytes: size,
                audio_group: None,
            },
        ));
    }

    if variants.is_empty() {
        return Err("Vortex couldn't find a video on that page.".to_owned());
    }
    let mut variants = one_per_resolution(variants);
    variants.sort_by_key(|v| (v.height.unwrap_or(0), v.bandwidth));
    default_audio(&mut audio, &audio_bandwidth);

    let default_variant = variants
        .iter()
        .enumerate()
        .min_by_key(|(index, v)| {
            (
                v.height.unwrap_or(0).abs_diff(preferred_height),
                std::cmp::Reverse(*index),
            )
        })
        .map(|(index, _)| index as u32);

    Ok(MediaCandidate {
        id: url.to_owned(),
        manifest_url: url.to_owned(),
        kind,
        title,
        duration_secs: duration,
        live,
        variants,
        audio,
        subtitles: Vec::new(),
        default_variant,
    })
}

/// How readily a codec plays back and muxes without a re-encode. Lower is better.
///
/// Only used to break a tie between formats of the *same resolution*, which is a choice a
/// ladder should not be making the user make. H.264 is first because it plays on every
/// device made this century and goes into an MP4 untouched.
fn codec_rank(vcodec: &str) -> u8 {
    let codec = vcodec.to_ascii_lowercase();
    match () {
        _ if codec.starts_with("avc") || codec.starts_with("h264") => 0,
        _ if codec.starts_with("vp9") || codec.starts_with("vp09") => 1,
        _ if codec.starts_with("av01") => 2,
        _ if codec.starts_with("hev") || codec.starts_with("hvc") => 3,
        _ => 4,
    }
}

/// One rung per resolution.
///
/// An extractor reports every encode a site publishes, and a site like YouTube publishes
/// three of each — H.264, VP9 and AV1 at 144p, at 240p, at 360p, all the way up. Handed
/// through unchanged that is a panel of twenty-five rows with `1080p` written on three of
/// them, which is not a choice, it is a quiz. So each resolution keeps one format: the most
/// playable codec, and the better encode where the codec ties.
///
/// **A format with no stated resolution is passed through untouched.** Collapsing those
/// would fold an entire ladder of unknown heights into a single rung.
fn one_per_resolution(scored: Vec<(u8, MediaVariant)>) -> Vec<MediaVariant> {
    let mut best: std::collections::BTreeMap<u32, (u8, MediaVariant)> =
        std::collections::BTreeMap::new();
    let mut unsized_variants = Vec::new();

    for (rank, variant) in scored {
        let Some(height) = variant.height else {
            unsized_variants.push(variant);
            continue;
        };
        let better = match best.get(&height) {
            None => true,
            // Lower rank wins; at equal rank the bigger bitrate is the better encode.
            Some((kept_rank, kept)) => (rank, kept.bandwidth) < (*kept_rank, variant.bandwidth),
        };
        if better {
            best.insert(height, (rank, variant));
        }
    }

    let mut kept: Vec<MediaVariant> = best.into_values().map(|(_, variant)| variant).collect();
    kept.append(&mut unsized_variants);
    kept
}

/// Marks the audio track the overlay should submit when the user does not choose one.
///
/// Without this every track is `default: false` and the consumer takes the first one the
/// extractor happened to list, which on YouTube is the *lowest* bitrate — a 4K download
/// with 48 kbps audio. The pick is the loudest track in the first language listed, because
/// extractors put the original language first and the alternatives are dubs: taking the
/// highest bitrate outright would hand someone a Portuguese dub of an English talk.
fn default_audio(audio: &mut [MediaTrack], bandwidth: &[u64]) {
    let Some(original) = audio.first().map(|track| track.language.clone()) else {
        return;
    };
    let best = audio
        .iter()
        .enumerate()
        .filter(|(_, track)| track.language == original)
        .max_by_key(|(index, _)| bandwidth.get(*index).copied().unwrap_or(0))
        .map(|(index, _)| index);
    if let Some(index) = best {
        audio[index].default = true;
    }
}

fn codec_label(vcodec: &str, acodec: &str) -> Option<String> {
    match (vcodec, acodec) {
        ("none", "none") => None,
        (v, "none") => Some(v.to_owned()),
        (v, a) => Some(format!("{v},{a}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"{
      "title": "A Conference Talk",
      "duration": 1800.0,
      "is_live": false,
      "formats": [
        {"format_id":"140","url":"https://cdn.example/audio.m4a","protocol":"https","vcodec":"none","acodec":"mp4a.40.2","tbr":128.0,"filesize":28800000,"format_note":"medium"},
        {"format_id":"137","url":"https://cdn.example/v1080.mp4","protocol":"https","vcodec":"avc1.640028","acodec":"none","width":1920,"height":1080,"tbr":4400.0},
        {"format_id":"18","url":"https://cdn.example/muxed360.mp4","protocol":"https","vcodec":"avc1.42001E","acodec":"mp4a.40.2","width":640,"height":360,"tbr":700.0},
        {"format_id":"rtmp","url":"rtmp://legacy.example/live","protocol":"rtmp","vcodec":"avc1","acodec":"aac","height":720}
      ]
    }"#;

    #[test]
    fn a_dump_becomes_a_ladder_with_its_audio_kept_separate() {
        let candidate = parse(DUMP, "https://example.com/watch", 1080).unwrap();
        assert_eq!(candidate.title, "A Conference Talk");
        assert_eq!(candidate.duration_secs, Some(1800.0));
        assert_eq!(candidate.kind, MediaKind::Progressive);
        assert_eq!(candidate.variants.len(), 2, "rtmp is not a protocol we speak");
        assert_eq!(candidate.variants[0].height, Some(360));
        assert_eq!(candidate.variants[1].height, Some(1080));
        assert_eq!(candidate.default_variant, Some(1));
        assert_eq!(candidate.audio.len(), 1);
        assert_eq!(candidate.audio[0].estimated_bytes, Some(28_800_000));
        // The id is the resolved URL, which is the whole point: our scheduler fetches it.
        assert_eq!(candidate.variants[1].id, "https://cdn.example/v1080.mp4");
    }

    /// The shape YouTube actually returns: three codecs at every resolution, and a stack
    /// of audio formats from worst to best with a dub at the end.
    const YOUTUBE: &str = r#"{
      "title": "Big Buck Bunny",
      "duration": 634.0,
      "formats": [
        {"url":"https://cdn/a48","protocol":"https","vcodec":"none","acodec":"opus","tbr":48.0,"language":"en","format_note":"low"},
        {"url":"https://cdn/a128","protocol":"https","vcodec":"none","acodec":"mp4a.40.2","tbr":128.0,"language":"en","format_note":"medium"},
        {"url":"https://cdn/a-pt","protocol":"https","vcodec":"none","acodec":"mp4a.40.2","tbr":384.0,"language":"pt","format_note":"dubbed"},
        {"url":"https://cdn/720-av01","protocol":"https","vcodec":"av01.0.08M.08","acodec":"none","height":720,"tbr":900.0},
        {"url":"https://cdn/720-avc1","protocol":"https","vcodec":"avc1.4d401f","acodec":"none","height":720,"tbr":1200.0},
        {"url":"https://cdn/720-vp9","protocol":"https","vcodec":"vp9","acodec":"none","height":720,"tbr":1000.0},
        {"url":"https://cdn/1080-vp9","protocol":"https","vcodec":"vp9","acodec":"none","height":1080,"tbr":2000.0},
        {"url":"https://cdn/1080-av01","protocol":"https","vcodec":"av01.0.09M.08","acodec":"none","height":1080,"tbr":1800.0}
      ]
    }"#;

    #[test]
    fn a_resolution_gets_one_rung_and_not_one_per_codec() {
        // An extractor reports every encode a site publishes. Handed through unchanged
        // that is a panel with `720p` written on three rows, which is not a choice.
        let candidate = parse(YOUTUBE, "https://example.com/watch", 1080).unwrap();
        let rungs: Vec<_> = candidate
            .variants
            .iter()
            .map(|v| (v.height, v.id.as_str()))
            .collect();
        assert_eq!(
            rungs,
            vec![
                (Some(720), "https://cdn/720-avc1"),
                (Some(1080), "https://cdn/1080-vp9"),
            ],
            "H.264 wins its resolution outright; VP9 wins the one H.264 does not publish"
        );
    }

    #[test]
    fn the_default_audio_is_the_best_one_in_the_original_language() {
        // Taking the first track is a 4K download with 48 kbps audio. Taking the loudest
        // outright is a Portuguese dub of an English talk.
        let candidate = parse(YOUTUBE, "https://example.com/watch", 1080).unwrap();
        let default: Vec<_> = candidate
            .audio
            .iter()
            .filter(|track| track.default)
            .map(|track| track.id.as_str())
            .collect();
        assert_eq!(default, vec!["https://cdn/a128"]);
    }

    #[test]
    fn a_ladder_of_unknown_heights_is_left_alone() {
        // Collapsing formats with no stated resolution would fold a whole ladder into one
        // rung, on the grounds that nothing said they were different.
        let dump = r#"{"title":"x","formats":[
          {"url":"https://cdn/a","protocol":"https","vcodec":"avc1","acodec":"none","tbr":800.0},
          {"url":"https://cdn/b","protocol":"https","vcodec":"avc1","acodec":"none","tbr":1600.0}]}"#;
        let candidate = parse(dump, "https://example.com/watch", 1080).unwrap();
        assert_eq!(candidate.variants.len(), 2);
    }

    #[test]
    fn an_hls_format_hands_the_manifest_back_to_our_own_parser() {
        let dump = r#"{"title":"Live","is_live":true,"formats":[
          {"url":"https://cdn.example/master.m3u8","protocol":"m3u8_native","vcodec":"avc1","acodec":"mp4a","height":720,"tbr":2500.0}]}"#;
        let candidate = parse(dump, "https://example.com/live", 1080).unwrap();
        assert_eq!(candidate.kind, MediaKind::Hls);
        assert!(candidate.live);
        assert_eq!(candidate.variants[0].id, "https://cdn.example/master.m3u8");
    }

    #[test]
    fn a_page_with_nothing_playable_says_so_rather_than_returning_an_empty_ladder() {
        let error = parse(r#"{"title":"An Article","formats":[]}"#, "https://x/y", 1080).unwrap_err();
        assert_eq!(error, "Vortex couldn't find a video on that page.");
        assert!(parse("not json", "https://x/y", 1080).is_err());
    }

    #[test]
    fn only_non_identifying_headers_are_forwarded() {
        assert!(FORWARDED.contains(&"user-agent"));
        assert!(FORWARDED.contains(&"referer"));
        assert!(!FORWARDED.contains(&"cookie"), "the cookie jar never leaves the daemon");
    }
}
