//! Stages 2–4: the ladder, the selection, and the concrete segment list (04 §1–4).
//!
//! Two things here are worth more than the rest of the module put together:
//!
//! * **Audio is a separate track.** In HLS it lives in an `EXT-X-MEDIA` rendition, in DASH
//!   in its own `AdaptationSet`. A downloader that resolves only the video variant produces
//!   a silent file, which is the single most common failure of naive implementations and
//!   therefore the acceptance criterion for this phase (06 §Phase 4).
//! * **The default is not the maximum.** Someone saving a lecture wants the resolution they
//!   were watching, not the 4 GB HEVC master (04 §3).

use crate::hls::{self, RenditionKind};
use crate::{dash, drm, net, Error, Resource, Result};
use vortex_engine::job::Engine;
use vortex_proto::{
    ContainerPreference, MediaCandidate, MediaKind, MediaSelection, MediaTrack, MediaVariant,
    RequestEnvelope,
};

/// What the ladder is built from, kept around so a selection does not have to re-guess the
/// manifest's kind.
#[derive(Debug, Clone)]
pub enum Manifest {
    Hls(Box<hls::Master>),
    /// A media playlist reached directly: one variant, no ladder.
    HlsMedia(Box<hls::Media>),
    Dash(Box<dash::Mpd>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Video,
    Audio,
    Subtitle,
}

/// How a track's segments become one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    /// Byte-append. Correct for MPEG-TS and for fMP4 behind an init segment.
    Append,
    /// Segmented WebVTT, which repeats its header and carries a per-segment timestamp map.
    /// Concatenating it produces a file players reject (see [`crate::vtt`]).
    WebVtt,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub role: Role,
    pub language: Option<String>,
    pub codecs: Option<String>,
    /// `EXT-X-MAP` / DASH `Initialization`. Omit it on fMP4 and nothing plays.
    pub init: Option<Resource>,
    pub segments: Vec<Segment>,
    /// Filename inside the job's work directory, extension included so ffmpeg can sniff.
    pub file: String,
    pub estimated_bytes: Option<u64>,
    /// Trailing segments that are guesses. See [`crate::dash::Representation::speculative`].
    pub speculative: u32,
    pub payload: Payload,
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub resource: Resource,
    pub key: Option<Key>,
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub url: String,
    pub iv: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mkv,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mkv => "mkv",
        }
    }
}

/// Everything the job needs to fetch and mux one stream.
#[derive(Debug, Clone)]
pub struct Plan {
    pub title: String,
    pub kind: MediaKind,
    pub live: bool,
    pub duration_secs: Option<f64>,
    pub tracks: Vec<Track>,
    pub container: Container,
    /// e.g. "Saved as MKV — MP4 can't hold Opus audio." Shown, never silent (04 §7).
    pub container_note: Option<String>,
}

impl Plan {
    /// Segments the manifest actually declares. The speculative tail is deliberately not
    /// counted: a progress bar must not sit at 94% because the estimate overshot.
    pub fn segments(&self) -> u32 {
        self.tracks
            .iter()
            .map(|t| {
                (t.segments.len() as u32).saturating_sub(t.speculative)
                    + u32::from(t.init.is_some())
            })
            .sum()
    }

    pub fn estimated_bytes(&self) -> Option<u64> {
        let total: u64 = self.tracks.iter().filter_map(|t| t.estimated_bytes).sum();
        (total > 0).then_some(total)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1–3. Fetch, ladder, default selection
// ─────────────────────────────────────────────────────────────────────────────

/// Fetches a manifest and describes what is in it. This is what the page overlay renders,
/// so it does the minimum number of requests that still yields an honest duration: the
/// master playlist, plus one media playlist to learn how long the thing is.
pub async fn inspect(
    engine: &Engine,
    envelope: &RequestEnvelope,
    preferred_height: u32,
) -> Result<MediaCandidate> {
    let url = envelope.effective_url().to_owned();
    let (text, fetched) = net::get_text(engine, envelope, &url).await?;
    let manifest = read(&text, &fetched.final_url, fetched.content_type.as_deref())?;

    match manifest {
        Manifest::Hls(master) => hls_candidate(engine, envelope, &fetched.final_url, *master, preferred_height).await,
        Manifest::HlsMedia(media) => {
            if let Err(refusal) = hls::encryption(&media) {
                return Err(Error::Refused(refusal));
            }
            unsupported_encryption(&media)?;
            let duration = media.duration();
            Ok(MediaCandidate {
                id: fetched.final_url.clone(),
                manifest_url: fetched.final_url.clone(),
                kind: MediaKind::Hls,
                title: title_from(envelope, &fetched.final_url),
                duration_secs: (duration > 0.0).then_some(duration),
                live: media.live,
                variants: vec![MediaVariant {
                    id: fetched.final_url.clone(),
                    width: None,
                    height: None,
                    bandwidth: 0,
                    codec_label: None,
                    estimated_bytes: None,
                    audio_group: None,
                }],
                audio: Vec::new(),
                subtitles: Vec::new(),
                default_variant: Some(0),
            })
        }
        Manifest::Dash(mpd) => Ok(dash_candidate(envelope, &fetched.final_url, *mpd, preferred_height)),
    }
}

/// Decides what a fetched body is. The content type is a hint, not an answer: plenty of
/// servers send `application/octet-stream` for an `.m3u8`.
pub fn read(text: &str, url: &str, content_type: Option<&str>) -> Result<Manifest> {
    if hls::looks_like_hls(text) {
        return if hls::is_master(text) {
            hls::parse_master(text, url)
                .map(|m| Manifest::Hls(Box::new(m)))
                .map_err(|e| Error::Unreadable(format!("that playlist would not parse: {e}")))
        } else {
            hls::parse_media(text, url)
                .map(|m| Manifest::HlsMedia(Box::new(m)))
                .map_err(|e| Error::Unreadable(format!("that playlist would not parse: {e}")))
        };
    }
    if text.contains("<MPD") || content_type.is_some_and(|t| t.contains("dash+xml")) {
        return match dash::parse(text, url) {
            Ok(Ok(mpd)) => Ok(Manifest::Dash(Box::new(mpd))),
            Ok(Err(refusal)) => Err(Error::Refused(refusal)),
            Err(e) => Err(Error::Unreadable(format!("that manifest would not parse: {e}"))),
        };
    }
    Err(Error::Unreadable(
        "That link isn't a stream Vortex can read.".to_owned(),
    ))
}

async fn hls_candidate(
    engine: &Engine,
    envelope: &RequestEnvelope,
    url: &str,
    master: hls::Master,
    preferred_height: u32,
) -> Result<MediaCandidate> {
    let default_variant = nearest_height(&master.variants, preferred_height);

    // One extra request buys a real duration for the whole ladder, and is also where an
    // encrypted stream announces itself.
    let mut duration = None;
    let mut live = false;
    if let Some(variant) = default_variant.and_then(|i| master.variants.get(i)) {
        let (text, fetched) = net::get_text(engine, envelope, &variant.url).await?;
        if let Ok(media) = hls::parse_media(&text, &fetched.final_url) {
            if let Err(refusal) = hls::encryption(&media) {
                return Err(Error::Refused(refusal));
            }
            unsupported_encryption(&media)?;
            let seconds = media.duration();
            duration = (seconds > 0.0).then_some(seconds);
            live = media.live;
        }
    }

    let variants = master
        .variants
        .iter()
        .map(|v| MediaVariant {
            id: v.url.clone(),
            width: v.width,
            height: v.height,
            bandwidth: v.bandwidth,
            codec_label: v.codecs.clone(),
            estimated_bytes: estimate(v.average_bandwidth.unwrap_or(v.bandwidth), duration),
            audio_group: v.audio_group.clone(),
        })
        .collect();

    let audio_group = default_variant
        .and_then(|i| master.variants.get(i))
        .and_then(|v| v.audio_group.clone());
    let audio = master
        .renditions
        .iter()
        .filter(|r| r.kind == RenditionKind::Audio && r.url.is_some())
        .filter(|r| audio_group.as_deref().is_none_or(|g| r.group_id == g))
        .map(|r| MediaTrack {
            id: r.url.clone().unwrap_or_default(),
            label: rendition_label(r),
            language: r.language.clone(),
            codec_label: None,
            estimated_bytes: None,
            default: r.default,
        })
        .collect();
    let subtitles = master
        .renditions
        .iter()
        .filter(|r| r.kind == RenditionKind::Subtitles && r.url.is_some())
        .map(|r| MediaTrack {
            id: r.url.clone().unwrap_or_default(),
            label: rendition_label(r),
            language: r.language.clone(),
            codec_label: None,
            estimated_bytes: None,
            default: r.default,
        })
        .collect();

    Ok(MediaCandidate {
        id: url.to_owned(),
        manifest_url: url.to_owned(),
        kind: MediaKind::Hls,
        title: title_from(envelope, url),
        duration_secs: duration,
        live,
        variants,
        audio,
        subtitles,
        default_variant: default_variant.map(|i| i as u32),
    })
}

fn dash_candidate(
    envelope: &RequestEnvelope,
    url: &str,
    mpd: dash::Mpd,
    preferred_height: u32,
) -> MediaCandidate {
    let reps = dash::ladder(&mpd);
    let duration = mpd.duration;
    let mut video: Vec<&dash::Representation> = reps
        .iter()
        .filter(|r| r.content_type == dash::ContentType::Video)
        .collect();
    video.sort_by_key(|r| r.bandwidth);

    let default_variant = video
        .iter()
        .enumerate()
        .min_by_key(|(_, r)| r.height.unwrap_or(0).abs_diff(preferred_height))
        .map(|(i, _)| i as u32);

    MediaCandidate {
        id: url.to_owned(),
        manifest_url: url.to_owned(),
        kind: MediaKind::Dash,
        title: mpd.title.clone().unwrap_or_else(|| title_from(envelope, url)),
        duration_secs: duration,
        live: mpd.dynamic,
        variants: video
            .iter()
            .map(|r| MediaVariant {
                id: r.id.clone(),
                width: r.width,
                height: r.height,
                bandwidth: r.bandwidth,
                codec_label: r.codecs.clone(),
                estimated_bytes: estimate(r.bandwidth, duration),
                audio_group: None,
            })
            .collect(),
        audio: reps
            .iter()
            .filter(|r| r.content_type == dash::ContentType::Audio)
            .map(|r| MediaTrack {
                id: r.id.clone(),
                label: audio_label(r),
                language: r.language.clone(),
                codec_label: r.codecs.clone(),
                estimated_bytes: estimate(r.bandwidth, duration),
                default: r.role_main,
            })
            .collect(),
        subtitles: reps
            .iter()
            .filter(|r| r.content_type == dash::ContentType::Text)
            .map(|r| MediaTrack {
                id: r.id.clone(),
                label: r.language.clone().unwrap_or_else(|| "Subtitles".to_owned()),
                language: r.language.clone(),
                codec_label: r.codecs.clone(),
                estimated_bytes: None,
                default: r.role_main,
            })
            .collect(),
        default_variant,
    }
}

/// The variant nearest the height the user was actually watching. Ties go to the higher
/// one, because "nearest 1080" landing on 720 reads as a downgrade.
fn nearest_height(variants: &[hls::Variant], preferred: u32) -> Option<usize> {
    if variants.is_empty() {
        return None;
    }
    variants
        .iter()
        .enumerate()
        .min_by_key(|(index, v)| {
            (
                v.height.unwrap_or(0).abs_diff(preferred),
                std::cmp::Reverse(*index),
            )
        })
        .map(|(index, _)| index)
}

/// `bandwidth × duration / 8`, a peak declaration the UI marks with `~` (04 §1-2).
fn estimate(bandwidth: u64, duration: Option<f64>) -> Option<u64> {
    let duration = duration?;
    (bandwidth > 0 && duration > 0.0).then(|| (bandwidth as f64 * duration / 8.0) as u64)
}

fn rendition_label(r: &hls::Rendition) -> String {
    match (r.name.is_empty(), &r.language) {
        (false, _) => r.name.clone(),
        (true, Some(language)) => language.clone(),
        (true, None) => r.group_id.clone(),
    }
}

fn audio_label(r: &dash::Representation) -> String {
    let language = r.language.clone().unwrap_or_else(|| "Audio".to_owned());
    match r.channels {
        Some(channels) if channels > 2 => format!("{language} · {channels}ch"),
        _ => language,
    }
}

fn title_from(envelope: &RequestEnvelope, url: &str) -> String {
    envelope
        .page_title
        .as_deref()
        .map(vortex_engine::naming::from_page_title)
        .filter(|t| !t.is_empty())
        .or_else(|| vortex_engine::naming::from_url(url).map(|n| strip_extension(&n)))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "video".to_owned())
}

fn strip_extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}

/// Clear-key `SAMPLE-AES`: legal, and not implemented. Said plainly rather than dressed up
/// as a DRM refusal (see [`crate::drm`]).
fn unsupported_encryption(media: &hls::Media) -> Result<()> {
    for segment in &media.segments {
        if let Some(key) = &segment.key {
            drm::check_supported(key.method)?;
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Expands a selection into the ordered list of segments that will actually be fetched.
///
/// The manifest is re-fetched rather than cached from `inspect`: signed segment URLs expire
/// in minutes, and the gap between "the user saw the ladder" and "the job starts" can be
/// hours if it sat in a queue.
pub async fn resolve(
    engine: &Engine,
    envelope: &RequestEnvelope,
    selection: &MediaSelection,
    preference: ContainerPreference,
) -> Result<Plan> {
    let (text, fetched) = net::get_text(engine, envelope, &selection.manifest_url).await?;
    match read(&text, &fetched.final_url, fetched.content_type.as_deref())? {
        Manifest::Hls(master) => {
            resolve_hls(engine, envelope, &master, selection, preference).await
        }
        // A media playlist reached directly is a one-variant ladder with no renditions.
        Manifest::HlsMedia(media) => {
            if let Err(refusal) = hls::encryption(&media) {
                return Err(Error::Refused(refusal));
            }
            unsupported_encryption(&media)?;
            let duration = Some(media.duration()).filter(|seconds| *seconds > 0.0);
            let track = track_from_media(Role::Video, &media, None, None, "video", None);
            let (container, container_note) =
                choose_container(std::slice::from_ref(&track), None, preference);
            Ok(Plan {
                title: selection.title.clone(),
                kind: MediaKind::Hls,
                live: media.live,
                duration_secs: duration,
                tracks: vec![track],
                container,
                container_note,
            })
        }
        Manifest::Dash(mpd) => resolve_dash(&mpd, selection, preference),
    }
}

async fn resolve_hls(
    engine: &Engine,
    envelope: &RequestEnvelope,
    master: &hls::Master,
    selection: &MediaSelection,
    preference: ContainerPreference,
) -> Result<Plan> {
    let variant = master
        .variants
        .iter()
        .find(|v| v.url == selection.variant_id)
        .ok_or_else(|| {
            Error::Unreadable("That quality is no longer offered for this video.".to_owned())
        })?;

    let (media, _) = fetch_media(engine, envelope, &variant.url).await?;
    let live = media.live;
    let duration = Some(media.duration()).filter(|seconds| *seconds > 0.0);
    let mut tracks = Vec::new();
    tracks.push(track_from_media(
        Role::Video,
        &media,
        variant.codecs.clone(),
        None,
        "video",
        estimate(variant.average_bandwidth.unwrap_or(variant.bandwidth), duration),
    ));

    if let Some(audio_id) = &selection.audio_id {
        let rendition = master
            .renditions
            .iter()
            .find(|r| r.url.as_deref() == Some(audio_id.as_str()));
        let (media, _) = fetch_media(engine, envelope, audio_id).await?;
        tracks.push(track_from_media(
            Role::Audio,
            &media,
            None,
            rendition.and_then(|r| r.language.clone()),
            "audio",
            None,
        ));
    }

    for (index, id) in selection.subtitle_ids.iter().enumerate() {
        let rendition = master
            .renditions
            .iter()
            .find(|r| r.url.as_deref() == Some(id.as_str()));
        // A subtitle rendition that fails to load must not take the video down with it.
        let Ok((media, _)) = fetch_media(engine, envelope, id).await else {
            tracing::warn!(id, "a subtitle rendition would not load; carrying on without it");
            continue;
        };
        tracks.push(track_from_media(
            Role::Subtitle,
            &media,
            None,
            rendition.and_then(|r| r.language.clone()),
            &format!("subtitle-{index}"),
            None,
        ));
    }

    let (container, note) = choose_container(&tracks, variant.codecs.as_deref(), preference);
    Ok(Plan {
        title: selection.title.clone(),
        kind: MediaKind::Hls,
        live,
        duration_secs: duration,
        tracks,
        container,
        container_note: note,
    })
}

async fn fetch_media(
    engine: &Engine,
    envelope: &RequestEnvelope,
    url: &str,
) -> Result<(hls::Media, String)> {
    let (text, fetched) = net::get_text(engine, envelope, url).await?;
    let media = hls::parse_media(&text, &fetched.final_url)
        .map_err(|e| Error::Unreadable(format!("that playlist would not parse: {e}")))?;
    if let Err(refusal) = hls::encryption(&media) {
        return Err(Error::Refused(refusal));
    }
    unsupported_encryption(&media)?;
    Ok((media, fetched.final_url))
}

fn track_from_media(
    role: Role,
    media: &hls::Media,
    codecs: Option<String>,
    language: Option<String>,
    stem: &str,
    estimated_bytes: Option<u64>,
) -> Track {
    let segments: Vec<Segment> = media
        .segments
        .iter()
        .map(|s| Segment {
            resource: s.resource.clone(),
            key: s.key.as_ref().map(|k| Key {
                url: k.url.clone(),
                iv: k.iv.unwrap_or_else(|| crate::crypto::iv_from_sequence(s.sequence)),
            }),
            duration: s.duration,
        })
        .collect();
    let extension = extension_for(role, media.init.is_some(), segments.first());
    Track {
        role,
        language,
        codecs,
        init: media.init.clone(),
        segments,
        file: format!("{stem}.{extension}"),
        estimated_bytes,
        // An HLS media playlist lists every segment it has. Nothing is guessed.
        speculative: 0,
        payload: payload_for(role, extension),
    }
}

fn payload_for(role: Role, extension: &str) -> Payload {
    match (role, extension) {
        (Role::Subtitle, "vtt") => Payload::WebVtt,
        _ => Payload::Append,
    }
}

/// ffmpeg sniffs, but only if the name does not actively lie. A concatenated fMP4 track is
/// an `.mp4`; a concatenated MPEG-TS track is a `.ts`; WebVTT segments are a `.vtt`.
fn extension_for(role: Role, has_init: bool, first: Option<&Segment>) -> &'static str {
    if role == Role::Subtitle {
        return "vtt";
    }
    if has_init {
        return "mp4";
    }
    let url = first.map(|s| s.resource.url.as_str()).unwrap_or_default();
    let path = url.split(['?', '#']).next().unwrap_or_default();
    match path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) {
        Some(ext) if ext == "m4s" || ext == "mp4" || ext == "m4a" || ext == "cmf" => "mp4",
        Some(ext) if ext == "aac" => "aac",
        Some(ext) if ext == "vtt" => "vtt",
        _ => "ts",
    }
}

fn resolve_dash(
    mpd: &dash::Mpd,
    selection: &MediaSelection,
    preference: ContainerPreference,
) -> Result<Plan> {
    let reps = dash::ladder(mpd);
    let video = dash::find(&reps, &selection.variant_id).ok_or_else(|| {
        Error::Unreadable("That quality is no longer offered for this video.".to_owned())
    })?;

    let mut tracks = vec![track_from_rep(Role::Video, video, "video", mpd.duration)];
    if let Some(audio_id) = &selection.audio_id {
        if let Some(rep) = dash::find(&reps, audio_id) {
            tracks.push(track_from_rep(Role::Audio, rep, "audio", mpd.duration));
        }
    }
    for (index, id) in selection.subtitle_ids.iter().enumerate() {
        if let Some(rep) = dash::find(&reps, id) {
            tracks.push(track_from_rep(
                Role::Subtitle,
                rep,
                &format!("subtitle-{index}"),
                mpd.duration,
            ));
        }
    }

    let (container, note) = choose_container(&tracks, video.codecs.as_deref(), preference);
    Ok(Plan {
        title: selection.title.clone(),
        kind: MediaKind::Dash,
        live: mpd.dynamic,
        duration_secs: mpd.duration,
        tracks,
        container,
        container_note: note,
    })
}

fn track_from_rep(
    role: Role,
    rep: &dash::Representation,
    stem: &str,
    duration: Option<f64>,
) -> Track {
    let segments: Vec<Segment> = rep
        .segments
        .iter()
        .map(|resource| Segment {
            resource: resource.clone(),
            key: None,
            duration: 0.0,
        })
        .collect();
    let extension = match (role, rep.mime.as_deref()) {
        (Role::Subtitle, Some(mime)) if mime.contains("ttml") => "ttml",
        (Role::Subtitle, _) => "vtt",
        _ => "mp4",
    };
    Track {
        role,
        language: rep.language.clone(),
        codecs: rep.codecs.clone(),
        init: rep.init.clone(),
        segments,
        file: format!("{stem}.{extension}"),
        estimated_bytes: estimate(rep.bandwidth, duration),
        speculative: rep.speculative,
        // DASH text representations are whole documents or `wvtt` inside fMP4; neither is
        // the segmented-WebVTT shape that needs stitching.
        payload: Payload::Append,
    }
}

/// MP4 when the codecs allow it, MKV when they don't — with the reason shown. Silently
/// producing a file the user's player refuses to open is a support ticket (04 §7).
pub fn choose_container(
    tracks: &[Track],
    video_codecs: Option<&str>,
    preference: ContainerPreference,
) -> (Container, Option<String>) {
    match preference {
        ContainerPreference::Mp4 => return (Container::Mp4, None),
        ContainerPreference::Mkv => return (Container::Mkv, None),
        ContainerPreference::Auto => {}
    }

    let codecs: String = tracks
        .iter()
        .filter_map(|t| t.codecs.as_deref())
        .chain(video_codecs)
        .collect::<Vec<_>>()
        .join(",")
        .to_ascii_lowercase();

    if codecs.contains("opus") {
        return (
            Container::Mkv,
            Some("Saved as MKV — MP4 can't hold Opus audio.".to_owned()),
        );
    }
    if codecs.contains("vorbis") {
        return (
            Container::Mkv,
            Some("Saved as MKV — MP4 can't hold Vorbis audio.".to_owned()),
        );
    }
    if codecs.contains("vp8") || codecs.contains("vp09") || codecs.contains("vp9") {
        return (
            Container::Mkv,
            Some("Saved as MKV — MP4 can't reliably hold VP9 video.".to_owned()),
        );
    }
    if tracks.iter().filter(|t| t.role == Role::Subtitle).count() > 1 {
        return (
            Container::Mkv,
            Some("Saved as MKV — MP4 can only carry one subtitle track well.".to_owned()),
        );
    }
    (Container::Mp4, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(height: u32) -> hls::Variant {
        hls::Variant {
            url: format!("https://x.example/{height}.m3u8"),
            bandwidth: height as u64 * 4000,
            average_bandwidth: None,
            width: None,
            height: Some(height),
            codecs: None,
            frame_rate: None,
            audio_group: None,
            subtitle_group: None,
        }
    }

    fn track(role: Role, codecs: Option<&str>) -> Track {
        Track {
            role,
            language: None,
            codecs: codecs.map(str::to_owned),
            init: None,
            segments: Vec::new(),
            file: "x".into(),
            estimated_bytes: None,
            speculative: 0,
            payload: Payload::Append,
        }
    }

    #[test]
    fn the_default_is_what_the_user_was_watching_not_the_maximum() {
        let ladder = [variant(360), variant(720), variant(1080), variant(2160)];
        assert_eq!(nearest_height(&ladder, 1080), Some(2));
        assert_eq!(nearest_height(&ladder, 480), Some(0), "nearest, not next-up");
        assert_eq!(nearest_height(&ladder, 4320), Some(3));
    }

    #[test]
    fn a_tie_resolves_upward() {
        // 540 is equidistant from 360 and 720; the higher one is the kinder default.
        let ladder = [variant(360), variant(720)];
        assert_eq!(nearest_height(&ladder, 540), Some(1));
    }

    #[test]
    fn opus_forces_mkv_and_says_why() {
        let tracks = [track(Role::Video, Some("vp09.00.10.08")), track(Role::Audio, Some("opus"))];
        let (container, note) = choose_container(&tracks, None, ContainerPreference::Auto);
        assert_eq!(container, Container::Mkv);
        assert_eq!(note.unwrap(), "Saved as MKV — MP4 can't hold Opus audio.");
    }

    #[test]
    fn h264_and_aac_stay_in_mp4_without_a_note() {
        let tracks = [
            track(Role::Video, Some("avc1.640028")),
            track(Role::Audio, Some("mp4a.40.2")),
        ];
        let (container, note) = choose_container(&tracks, None, ContainerPreference::Auto);
        assert_eq!(container, Container::Mp4);
        assert!(note.is_none());
    }

    #[test]
    fn an_explicit_preference_is_obeyed_without_editorialising() {
        let tracks = [track(Role::Audio, Some("opus"))];
        assert_eq!(
            choose_container(&tracks, None, ContainerPreference::Mp4),
            (Container::Mp4, None)
        );
    }

    #[test]
    fn the_estimate_is_bandwidth_times_duration_over_eight() {
        assert_eq!(estimate(8_000_000, Some(60.0)), Some(60_000_000));
        assert_eq!(estimate(8_000_000, None), None);
        assert_eq!(estimate(0, Some(60.0)), None);
    }

    #[test]
    fn a_track_is_named_for_what_ffmpeg_will_find_inside_it() {
        let ts = Segment {
            resource: Resource::whole("https://x.example/a/seg1.ts?token=1"),
            key: None,
            duration: 4.0,
        };
        assert_eq!(extension_for(Role::Video, false, Some(&ts)), "ts");
        assert_eq!(extension_for(Role::Video, true, Some(&ts)), "mp4");
        assert_eq!(extension_for(Role::Subtitle, false, None), "vtt");
    }
}
