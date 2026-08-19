//! HLS (RFC 8216). Our own parser, not a wrapper.
//!
//! The manifest *is* the product surface — the ladder in the page overlay comes straight
//! out of it — so this is deliberately first-party. What matters most is the part naive
//! downloaders skip: `EXT-X-MEDIA` renditions. A variant's audio usually lives in a
//! *separate* playlist, and a downloader that fetches only the `EXT-X-STREAM-INF` URI
//! produces a silent video and a confused user (04 §1).

use crate::drm;
use crate::Resource;
use std::collections::HashMap;
use url::Url;

/// A parsed master playlist: the ladder, plus every rendition it references.
#[derive(Debug, Clone, Default)]
pub struct Master {
    pub variants: Vec<Variant>,
    pub renditions: Vec<Rendition>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    /// Absolute URL of the media playlist.
    pub url: String,
    pub bandwidth: u64,
    pub average_bandwidth: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Option<String>,
    pub frame_rate: Option<f64>,
    /// `AUDIO="group"` — the renditions that carry this variant's sound.
    pub audio_group: Option<String>,
    pub subtitle_group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenditionKind {
    Audio,
    Subtitles,
    ClosedCaptions,
    Video,
}

#[derive(Debug, Clone)]
pub struct Rendition {
    pub kind: RenditionKind,
    pub group_id: String,
    pub name: String,
    pub language: Option<String>,
    /// Absent for `CLOSED-CAPTIONS`, which are muxed into the video and not fetchable.
    pub url: Option<String>,
    pub default: bool,
    pub autoselect: bool,
    pub channels: Option<u32>,
    pub forced: bool,
}

/// A parsed media playlist.
#[derive(Debug, Clone, Default)]
pub struct Media {
    pub target_duration: f64,
    pub media_sequence: u64,
    /// No `#EXT-X-ENDLIST`: the playlist is still growing.
    pub live: bool,
    pub playlist_type: Option<String>,
    /// `#EXT-X-MAP`. Omit it on fMP4 and the output is unplayable.
    pub init: Option<Resource>,
    pub segments: Vec<Segment>,
}

impl Media {
    pub fn duration(&self) -> f64 {
        self.segments.iter().map(|s| s.duration).sum()
    }
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub resource: Resource,
    pub duration: f64,
    /// Media sequence number. The AES-128 IV defaults to this when `#EXT-X-KEY` omits one.
    pub sequence: u64,
    pub discontinuity: bool,
    pub key: Option<Key>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub method: Method,
    pub url: String,
    pub iv: Option<[u8; 16]>,
    pub key_format: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    None,
    Aes128,
    SampleAes,
}

/// Master or media? Both start `#EXTM3U`, and guessing from the URL is how you end up
/// treating a ladder as a segment list.
pub fn is_master(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("#EXT-X-STREAM-INF") || l.starts_with("#EXT-X-MEDIA:"))
}

pub fn looks_like_hls(text: &str) -> bool {
    text.trim_start().starts_with("#EXTM3U")
}

pub fn parse_master(text: &str, base: &str) -> anyhow::Result<Master> {
    let base = Url::parse(base)?;
    let mut master = Master::default();
    let mut pending: Option<HashMap<String, String>> = None;

    for line in lines(text) {
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(attributes(rest));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = attributes(rest);
            let Some(kind) = attrs.get("TYPE").and_then(|t| rendition_kind(t)) else {
                continue;
            };
            master.renditions.push(Rendition {
                kind,
                group_id: attrs.get("GROUP-ID").cloned().unwrap_or_default(),
                name: attrs.get("NAME").cloned().unwrap_or_default(),
                language: attrs.get("LANGUAGE").cloned(),
                url: attrs
                    .get("URI")
                    .and_then(|u| base.join(u).ok())
                    .map(|u| u.to_string()),
                default: attrs.get("DEFAULT").is_some_and(|v| v == "YES"),
                autoselect: attrs.get("AUTOSELECT").is_some_and(|v| v == "YES"),
                // `CHANNELS` is "2" or "2/JOC"; only the count is ours to care about.
                channels: attrs
                    .get("CHANNELS")
                    .and_then(|c| c.split('/').next())
                    .and_then(|c| c.parse().ok()),
                forced: attrs.get("FORCED").is_some_and(|v| v == "YES"),
            });
            continue;
        }
        // I-frame variants are trick-play ladders — real video, no audio, useless as a
        // download. They are deliberately not offered.
        if line.starts_with('#') {
            continue;
        }
        let Some(attrs) = pending.take() else { continue };
        let Ok(url) = base.join(line) else { continue };
        let (width, height) = attrs
            .get("RESOLUTION")
            .and_then(|r| parse_resolution(r))
            .map_or((None, None), |(w, h)| (Some(w), Some(h)));
        master.variants.push(Variant {
            url: url.to_string(),
            bandwidth: attrs
                .get("BANDWIDTH")
                .and_then(|b| b.parse().ok())
                .unwrap_or(0),
            average_bandwidth: attrs.get("AVERAGE-BANDWIDTH").and_then(|b| b.parse().ok()),
            width,
            height,
            codecs: attrs.get("CODECS").cloned(),
            frame_rate: attrs.get("FRAME-RATE").and_then(|f| f.parse().ok()),
            audio_group: attrs.get("AUDIO").cloned(),
            subtitle_group: attrs.get("SUBTITLES").cloned(),
        });
    }

    master.variants.sort_by_key(|v| v.bandwidth);
    Ok(master)
}

pub fn parse_media(text: &str, base: &str) -> anyhow::Result<Media> {
    let base = Url::parse(base)?;
    let mut media = Media {
        live: true,
        ..Media::default()
    };
    let mut duration = 0.0f64;
    let mut discontinuity = false;
    let mut key: Option<Key> = None;
    let mut sequence = 0u64;
    let mut sequence_seen = false;
    let mut range: Option<(u64, u64)> = None;
    // Where the previous `EXT-X-BYTERANGE` ended, for the `length@` form that omits the
    // offset and means "immediately after the last one".
    let mut range_cursor: HashMap<String, u64> = HashMap::new();

    for line in lines(text) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            duration = rest
                .split(',')
                .next()
                .and_then(|d| d.trim().parse().ok())
                .unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            media.target_duration = rest.trim().parse().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = rest.trim().parse().unwrap_or(0);
            media.media_sequence = sequence;
            sequence_seen = true;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            media.playlist_type = Some(rest.trim().to_owned());
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            range = parse_byterange(rest.trim());
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = attributes(rest);
            if let Some(url) = attrs.get("URI").and_then(|u| base.join(u).ok()) {
                media.init = Some(Resource {
                    url: url.to_string(),
                    range: attrs.get("BYTERANGE").and_then(|r| parse_byterange(r)),
                });
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            key = parse_key(&attributes(rest), &base);
        } else if line.starts_with("#EXT-X-DISCONTINUITY") {
            discontinuity = true;
        } else if line.starts_with("#EXT-X-ENDLIST") {
            media.live = false;
        } else if !line.starts_with('#') {
            let Ok(url) = base.join(line) else { continue };
            let url = url.to_string();
            // `length@offset` with no offset continues where the last sub-range ended.
            let resolved = range.map(|(offset, length)| {
                let offset = if offset == u64::MAX {
                    range_cursor.get(&url).copied().unwrap_or(0)
                } else {
                    offset
                };
                range_cursor.insert(url.clone(), offset + length);
                (offset, length)
            });
            media.segments.push(Segment {
                resource: Resource {
                    url,
                    range: resolved,
                },
                duration,
                sequence,
                discontinuity,
                key: key.clone().filter(|k| k.method != Method::None),
            });
            sequence += 1;
            duration = 0.0;
            discontinuity = false;
            range = None;
        }
    }

    if !sequence_seen {
        // Without `EXT-X-MEDIA-SEQUENCE` the first segment is 0, which is also the default
        // AES-128 IV. Getting this wrong decrypts to noise rather than to an error.
        for (index, segment) in media.segments.iter_mut().enumerate() {
            segment.sequence = index as u64;
        }
    }
    Ok(media)
}

/// The encryption on this playlist, if any — and whether we are allowed to touch it.
pub fn encryption(media: &Media) -> Result<(), drm::Refusal> {
    for segment in &media.segments {
        if let Some(key) = &segment.key {
            drm::check_hls_key(key.method, key.key_format.as_deref())?;
        }
    }
    Ok(())
}

fn parse_key(attrs: &HashMap<String, String>, base: &Url) -> Option<Key> {
    let method = match attrs.get("METHOD").map(String::as_str) {
        Some("NONE") | None => Method::None,
        Some("AES-128") => Method::Aes128,
        Some("SAMPLE-AES") | Some("SAMPLE-AES-CTR") => Method::SampleAes,
        // An unknown METHOD is not something to guess at.
        Some(_) => Method::SampleAes,
    };
    if method == Method::None {
        return Some(Key {
            method,
            url: String::new(),
            iv: None,
            key_format: None,
        });
    }
    let url = attrs
        .get("URI")
        .and_then(|u| base.join(u).ok())
        .map(|u| u.to_string())?;
    Some(Key {
        method,
        url,
        iv: attrs.get("IV").and_then(|iv| parse_iv(iv)),
        key_format: attrs.get("KEYFORMAT").cloned(),
    })
}

fn parse_iv(text: &str) -> Option<[u8; 16]> {
    let hex_text = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    let bytes = hex::decode(hex_text).ok()?;
    (bytes.len() == 16).then(|| {
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&bytes);
        iv
    })
}

/// `length@offset`, or `length` alone meaning "continue from the last one" — signalled
/// here as `u64::MAX` and resolved by the caller, which is the only place that knows which
/// URI the run belongs to.
fn parse_byterange(text: &str) -> Option<(u64, u64)> {
    let text = text.trim().trim_matches('"');
    let (length, offset) = match text.split_once('@') {
        Some((length, offset)) => (length, offset.parse().ok()?),
        None => (text, u64::MAX),
    };
    Some((offset, length.parse().ok()?))
}

fn parse_resolution(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once('x').or_else(|| text.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn rendition_kind(text: &str) -> Option<RenditionKind> {
    match text {
        "AUDIO" => Some(RenditionKind::Audio),
        "SUBTITLES" => Some(RenditionKind::Subtitles),
        "CLOSED-CAPTIONS" => Some(RenditionKind::ClosedCaptions),
        "VIDEO" => Some(RenditionKind::Video),
        _ => None,
    }
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty())
}

/// `KEY=VALUE,KEY="value,with,commas"`. Splitting on commas without honouring quotes is
/// the single most common way to mangle a `CODECS` attribute.
fn attributes(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut quoted = false;

    let mut flush = |key: &mut String, value: &mut String| {
        if !key.trim().is_empty() {
            out.insert(key.trim().to_owned(), value.trim().to_owned());
        }
        key.clear();
        value.clear();
    };

    for ch in text.chars() {
        match ch {
            '"' => quoted = !quoted,
            '=' if !in_value && !quoted => in_value = true,
            ',' if !quoted => {
                flush(&mut key, &mut value);
                in_value = false;
            }
            c if in_value => value.push(c),
            c => key.push(c),
        }
    }
    flush(&mut key, &mut value);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = r#"#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aac",NAME="English",DEFAULT=YES,AUTOSELECT=YES,LANGUAGE="en",CHANNELS="2",URI="audio/en/playlist.m3u8"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aac",NAME="Deutsch",DEFAULT=NO,LANGUAGE="de",CHANNELS="6",URI="audio/de/playlist.m3u8"
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="English",LANGUAGE="en",URI="subs/en.m3u8"
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS="avc1.4d401e,mp4a.40.2",AUDIO="aac",SUBTITLES="subs"
v0/playlist.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000,AVERAGE-BANDWIDTH=4200000,RESOLUTION=1920x1080,CODECS="avc1.640028,mp4a.40.2",AUDIO="aac",FRAME-RATE=59.94
v3/playlist.m3u8
#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100000,URI="iframe.m3u8"
"#;

    #[test]
    fn the_ladder_comes_out_in_order_with_its_audio_group() {
        let master = parse_master(MASTER, "https://cdn.example.com/hls/master.m3u8").unwrap();
        assert_eq!(master.variants.len(), 2, "an I-frame ladder is not a variant");
        assert_eq!(master.variants[0].height, Some(360));
        assert_eq!(master.variants[1].height, Some(1080));
        assert_eq!(
            master.variants[1].url,
            "https://cdn.example.com/hls/v3/playlist.m3u8"
        );
        assert_eq!(master.variants[1].average_bandwidth, Some(4_200_000));
        // The comma inside CODECS must not have split the attribute list.
        assert_eq!(
            master.variants[1].codecs.as_deref(),
            Some("avc1.640028,mp4a.40.2")
        );
        assert_eq!(master.variants[1].audio_group.as_deref(), Some("aac"));
    }

    #[test]
    fn separate_audio_renditions_are_the_whole_point() {
        let master = parse_master(MASTER, "https://cdn.example.com/hls/master.m3u8").unwrap();
        let audio: Vec<_> = master
            .renditions
            .iter()
            .filter(|r| r.kind == RenditionKind::Audio)
            .collect();
        assert_eq!(audio.len(), 2);
        assert!(audio[0].default);
        assert_eq!(audio[0].language.as_deref(), Some("en"));
        assert_eq!(audio[1].channels, Some(6));
        assert_eq!(
            audio[0].url.as_deref(),
            Some("https://cdn.example.com/hls/audio/en/playlist.m3u8")
        );
    }

    #[test]
    fn a_media_playlist_carries_its_map_key_and_sequence() {
        let text = r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-TARGETDURATION:6
#EXT-X-MEDIA-SEQUENCE:100
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MAP:URI="init.mp4"
#EXT-X-KEY:METHOD=AES-128,URI="https://keys.example.com/k1",IV=0x0123456789ABCDEF0123456789ABCDEF
#EXTINF:6.0,
seg1.m4s
#EXT-X-DISCONTINUITY
#EXTINF:4.5,
seg2.m4s
#EXT-X-ENDLIST
"#;
        let media = parse_media(text, "https://cdn.example.com/hls/v3/playlist.m3u8").unwrap();
        assert!(!media.live);
        assert_eq!(media.target_duration, 6.0);
        assert_eq!(
            media.init.as_ref().unwrap().url,
            "https://cdn.example.com/hls/v3/init.mp4"
        );
        assert_eq!(media.segments.len(), 2);
        assert_eq!(media.segments[0].sequence, 100);
        assert_eq!(media.segments[1].sequence, 101);
        assert!(media.segments[1].discontinuity);
        assert_eq!((media.duration() * 10.0).round(), 105.0);
        let key = media.segments[0].key.as_ref().unwrap();
        assert_eq!(key.method, Method::Aes128);
        assert_eq!(key.iv.unwrap()[0], 0x01);
    }

    #[test]
    fn a_byterange_without_an_offset_continues_the_previous_one() {
        let text = r#"#EXTM3U
#EXT-X-TARGETDURATION:10
#EXTINF:10.0,
#EXT-X-BYTERANGE:1000@0
all.ts
#EXTINF:10.0,
#EXT-X-BYTERANGE:2000
all.ts
#EXT-X-ENDLIST
"#;
        let media = parse_media(text, "https://cdn.example.com/x/p.m3u8").unwrap();
        assert_eq!(media.segments[0].resource.range, Some((0, 1000)));
        assert_eq!(media.segments[1].resource.range, Some((1000, 2000)));
    }

    #[test]
    fn a_live_playlist_is_one_without_an_endlist() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,\na.ts\n";
        let media = parse_media(text, "https://x.example/p.m3u8").unwrap();
        assert!(media.live);
        assert_eq!(media.segments[0].sequence, 0);
    }

    #[test]
    fn a_master_is_told_from_a_media_playlist_by_its_tags_not_its_name() {
        assert!(is_master(MASTER));
        assert!(!is_master("#EXTM3U\n#EXTINF:4.0,\na.ts\n"));
        assert!(looks_like_hls("#EXTM3U\n"));
        assert!(!looks_like_hls("<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">"));
    }
}
