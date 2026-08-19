//! DASH (ISO/IEC 23009-1). The subset that real manifests actually use.
//!
//! Three things carry almost every VOD on the web: `SegmentTemplate` with `$Number$`,
//! `SegmentTemplate` with `$Time$` plus a `SegmentTimeline`, and `SegmentBase` with an
//! `indexRange` pointing at a `sidx`. All three are here. `SegmentTimeline` in particular
//! is not optional — `$Time$` values are media timestamps, and inferring them from a
//! nominal duration produces a file with silent gaps (04 §1-2).
//!
//! `ContentProtection` anywhere in the manifest ends the parse. See [`crate::drm`].

use crate::drm::{self, Refusal};
use crate::Resource;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::collections::HashMap;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Video,
    Audio,
    Text,
}

#[derive(Debug, Clone, Default)]
pub struct Mpd {
    /// `@type="dynamic"` — a live stream.
    pub dynamic: bool,
    pub duration: Option<f64>,
    pub title: Option<String>,
    pub periods: Vec<Period>,
}

#[derive(Debug, Clone, Default)]
pub struct Period {
    pub duration: Option<f64>,
    pub representations: Vec<Representation>,
}

#[derive(Debug, Clone)]
pub struct Representation {
    pub id: String,
    pub content_type: ContentType,
    pub mime: Option<String>,
    pub codecs: Option<String>,
    pub bandwidth: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub language: Option<String>,
    pub channels: Option<u32>,
    pub role_main: bool,
    pub init: Option<Resource>,
    pub segments: Vec<Resource>,
    /// A single-file representation whose segment boundaries live in a `sidx` box at this
    /// byte range. Resolved over the network later, because parsing cannot do it here.
    pub index: Option<Resource>,
    /// How many of the trailing segments are guesses rather than declarations.
    ///
    /// A `SegmentTemplate` with a nominal `@duration` and no `SegmentTimeline` does not say
    /// how many segments there are; the count is `duration / @duration`, and real encoders
    /// undershoot it — AAC frames do not divide into two-second segments evenly, so a
    /// six-second track routinely has four segments where the arithmetic says three.
    /// Trusting the arithmetic truncates the audio, which is the exact failure this whole
    /// pipeline exists to avoid. So the tail is over-generated and probed.
    pub speculative: u32,
}

impl Representation {
    pub fn label(&self) -> String {
        match (self.width, self.height) {
            (_, Some(h)) => format!("{h}p"),
            _ => format!("{} kbps", self.bandwidth / 1000),
        }
    }
}

/// Parses an MPD, or refuses it. A `Refusal` is not an error the user can retry past —
/// it is the product boundary (04 §The DRM boundary).
pub fn parse(text: &str, base: &str) -> anyhow::Result<Result<Mpd, Refusal>> {
    let base = Url::parse(base)?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);

    let mut mpd = Mpd::default();
    // Root level always exists so the manifest URL is the outermost base.
    let mut stack: Vec<Level> = vec![Level::default()];
    let mut capture: Option<Capture> = None;
    let mut period: Option<Period> = None;

    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) => {
                let name = local_name(&e);
                match name.as_str() {
                    "MPD" => {
                        read_mpd(&e, &mut mpd);
                        stack.last_mut().expect("root level").apply(&e);
                    }
                    "Period" => {
                        period = Some(Period {
                            // A single-period manifest routinely declares its length only
                            // on the MPD; without a duration a `$Number$` ladder has no
                            // segment count at all.
                            duration: attr(&e, "duration")
                                .and_then(|d| parse_duration(&d))
                                .or(mpd.duration),
                            representations: Vec::new(),
                        });
                        stack.push(Level::from(&e));
                    }
                    "AdaptationSet" | "Representation" => stack.push(Level::from(&e)),
                    "SegmentTemplate" | "SegmentList" | "SegmentBase" => {
                        if let Some(level) = stack.last_mut() {
                            level.segmenting(&name, &e);
                        }
                    }
                    "ContentProtection" => {
                        return Ok(Err(drm::check_dash_scheme(
                            &attr(&e, "schemeIdUri").unwrap_or_default(),
                        )
                        .unwrap_err()))
                    }
                    "BaseURL" => capture = Some(Capture::BaseUrl),
                    "Title" => capture = Some(Capture::Title),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = local_name(&e);
                match name.as_str() {
                    "SegmentTemplate" | "SegmentList" | "SegmentBase" => {
                        if let Some(level) = stack.last_mut() {
                            level.segmenting(&name, &e);
                        }
                    }
                    "ContentProtection" => {
                        return Ok(Err(drm::check_dash_scheme(
                            &attr(&e, "schemeIdUri").unwrap_or_default(),
                        )
                        .unwrap_err()))
                    }
                    "S" => {
                        if let Some(level) = stack.last_mut() {
                            level.timeline.push(TimelineEntry::from(&e));
                        }
                    }
                    "SegmentURL" => {
                        if let Some(level) = stack.last_mut() {
                            level.segment_urls.push((
                                attr(&e, "media").unwrap_or_default(),
                                attr(&e, "mediaRange"),
                            ));
                        }
                    }
                    "Initialization" => {
                        if let Some(level) = stack.last_mut() {
                            level.init = Some((
                                attr(&e, "sourceURL").unwrap_or_default(),
                                attr(&e, "range"),
                            ));
                        }
                    }
                    "Role" => {
                        if let Some(level) = stack.last_mut() {
                            level.role_main =
                                attr(&e, "value").is_some_and(|v| v == "main" || v == "alternate");
                        }
                    }
                    "AudioChannelConfiguration" => {
                        if let Some(level) = stack.last_mut() {
                            level.channels = attr(&e, "value").and_then(|v| v.parse().ok());
                        }
                    }
                    "Representation" => {
                        // Legal, if unusual: everything is inherited from the parent.
                        stack.push(Level::from(&e));
                        if let Some(rep) = build(&stack, &base, period.as_ref()) {
                            if let Some(p) = period.as_mut() {
                                p.representations.push(rep);
                            }
                        }
                        stack.pop();
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                let value = text.xml_content(XmlVersion::Implicit1_0)?.trim().to_owned();
                match capture.take() {
                    Some(Capture::BaseUrl) => {
                        if let Some(level) = stack.last_mut() {
                            level.base = Some(value);
                        }
                    }
                    Some(Capture::Title) => mpd.title = Some(value),
                    None => {}
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                match name.as_str() {
                    "Representation" => {
                        if let Some(rep) = build(&stack, &base, period.as_ref()) {
                            if let Some(p) = period.as_mut() {
                                p.representations.push(rep);
                            }
                        }
                        stack.pop();
                    }
                    "AdaptationSet" => {
                        stack.pop();
                    }
                    "Period" => {
                        stack.pop();
                        if let Some(p) = period.take() {
                            mpd.periods.push(p);
                        }
                    }
                    _ => {}
                }
                capture = None;
            }
            _ => {}
        }
    }

    Ok(Ok(mpd))
}

/// The ladder, with every period's segments concatenated onto the representation that
/// matches. Ad-inserted manifests repeat the same encoder ladder in each period; a
/// downloader that only reads the first period silently truncates the film.
pub fn ladder(mpd: &Mpd) -> Vec<Representation> {
    let Some(reference) = (0..mpd.periods.len())
        .max_by_key(|i| mpd.periods[*i].representations.len())
    else {
        return Vec::new();
    };
    let mut merged = mpd.periods[reference].representations.clone();
    if mpd.periods.len() < 2 {
        return merged;
    }
    for rep in &mut merged {
        let mut segments = Vec::new();
        for (index, period) in mpd.periods.iter().enumerate() {
            if index == reference {
                segments.append(&mut rep.segments);
            } else if let Some(peer) = match_in(period, rep) {
                segments.extend(peer.segments.iter().cloned());
            }
        }
        rep.segments = segments;
    }
    merged
}

/// The representation in `period` that continues `rep`: same id when the encoder kept it,
/// otherwise the nearest bitrate of the same kind.
fn match_in<'a>(period: &'a Period, rep: &Representation) -> Option<&'a Representation> {
    period
        .representations
        .iter()
        .find(|r| r.id == rep.id && r.content_type == rep.content_type)
        .or_else(|| {
            period
                .representations
                .iter()
                .filter(|r| r.content_type == rep.content_type && r.language == rep.language)
                .min_by_key(|r| r.bandwidth.abs_diff(rep.bandwidth))
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Inheritance
// ─────────────────────────────────────────────────────────────────────────────

enum Capture {
    BaseUrl,
    Title,
}

/// One level of the MPD tree. Almost everything in DASH is inherited from an ancestor and
/// overridden by a descendant, so resolution is "walk the stack, last one wins".
#[derive(Debug, Clone, Default)]
struct Level {
    base: Option<String>,
    id: Option<String>,
    mime: Option<String>,
    content_type: Option<String>,
    codecs: Option<String>,
    language: Option<String>,
    bandwidth: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    channels: Option<u32>,
    role_main: bool,
    template: Option<Template>,
    timeline: Vec<TimelineEntry>,
    segment_urls: Vec<(String, Option<String>)>,
    init: Option<(String, Option<String>)>,
    index_range: Option<String>,
    list_duration: Option<f64>,
}

impl Level {
    fn from(e: &BytesStart<'_>) -> Self {
        let mut level = Self::default();
        level.apply(e);
        level
    }

    fn apply(&mut self, e: &BytesStart<'_>) {
        self.id = attr(e, "id").or(self.id.take());
        self.mime = attr(e, "mimeType").or(self.mime.take());
        self.content_type = attr(e, "contentType").or(self.content_type.take());
        self.codecs = attr(e, "codecs").or(self.codecs.take());
        self.language = attr(e, "lang").or(self.language.take());
        self.bandwidth = attr(e, "bandwidth")
            .and_then(|b| b.parse().ok())
            .or(self.bandwidth);
        self.width = attr(e, "width").and_then(|w| w.parse().ok()).or(self.width);
        self.height = attr(e, "height")
            .and_then(|h| h.parse().ok())
            .or(self.height);
    }

    fn segmenting(&mut self, element: &str, e: &BytesStart<'_>) {
        match element {
            "SegmentTemplate" => self.template = Some(Template::from(e)),
            "SegmentList" => {
                self.list_duration = attr(e, "duration").and_then(|d| d.parse().ok());
                let timescale: f64 = attr(e, "timescale")
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(1.0);
                self.list_duration = self.list_duration.map(|d| d / timescale);
            }
            "SegmentBase" => self.index_range = attr(e, "indexRange"),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Template {
    media: Option<String>,
    initialization: Option<String>,
    start_number: u64,
    timescale: f64,
    duration: Option<f64>,
    presentation_time_offset: u64,
}

impl Template {
    fn from(e: &BytesStart<'_>) -> Self {
        Self {
            media: attr(e, "media"),
            initialization: attr(e, "initialization"),
            start_number: attr(e, "startNumber")
                .and_then(|n| n.parse().ok())
                .unwrap_or(1),
            timescale: attr(e, "timescale")
                .and_then(|t| t.parse().ok())
                .unwrap_or(1.0),
            duration: attr(e, "duration").and_then(|d| d.parse().ok()),
            presentation_time_offset: attr(e, "presentationTimeOffset")
                .and_then(|t| t.parse().ok())
                .unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimelineEntry {
    /// Media time of the first segment in this run. Absent means "where the last one ended".
    t: Option<u64>,
    d: u64,
    /// Additional repeats. `-1` means "until the period ends".
    r: i64,
}

impl TimelineEntry {
    fn from(e: &BytesStart<'_>) -> Self {
        Self {
            t: attr(e, "t").and_then(|t| t.parse().ok()),
            d: attr(e, "d").and_then(|d| d.parse().ok()).unwrap_or(0),
            r: attr(e, "r").and_then(|r| r.parse().ok()).unwrap_or(0),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Building one representation
// ─────────────────────────────────────────────────────────────────────────────

fn build(stack: &[Level], manifest: &Url, period: Option<&Period>) -> Option<Representation> {
    let base = resolve_base(stack, manifest);
    let id = last(stack, |l| l.id.clone()).unwrap_or_default();
    let bandwidth = last(stack, |l| l.bandwidth).unwrap_or(0);
    let mime = last(stack, |l| l.mime.clone());
    let codecs = last(stack, |l| l.codecs.clone());
    let content_type = content_type(
        last(stack, |l| l.content_type.clone()).as_deref(),
        mime.as_deref(),
        codecs.as_deref(),
    )?;

    let template = last(stack, |l| l.template.clone());
    let timeline: Vec<TimelineEntry> = stack
        .iter()
        .rev()
        .find(|l| !l.timeline.is_empty())
        .map(|l| l.timeline.clone())
        .unwrap_or_default();
    let segment_urls = stack
        .iter()
        .rev()
        .find(|l| !l.segment_urls.is_empty())
        .map(|l| l.segment_urls.clone())
        .unwrap_or_default();
    let explicit_init = last(stack, |l| l.init.clone());
    let index_range = last(stack, |l| l.index_range.clone());
    let duration = period.and_then(|p| p.duration);

    let mut init = explicit_init.and_then(|(source, range)| {
        let url = if source.is_empty() {
            base.to_string()
        } else {
            base.join(&source).ok()?.to_string()
        };
        Some(Resource {
            url,
            range: range.as_deref().and_then(parse_range),
        })
    });
    let mut segments = Vec::new();
    let mut index = None;
    let mut speculative = 0;

    if let Some(template) = &template {
        if let Some(pattern) = &template.initialization {
            let expanded = expand(pattern, &id, bandwidth, 0, 0);
            if let Ok(url) = base.join(&expanded) {
                init = Some(Resource {
                    url: url.to_string(),
                    range: None,
                });
            }
        }
        if let Some(pattern) = &template.media {
            let expanded =
                expand_template(pattern, template, &timeline, &id, bandwidth, duration, &base);
            speculative = expanded.speculative;
            segments = expanded.segments;
        }
    } else if !segment_urls.is_empty() {
        for (media, range) in &segment_urls {
            if let Ok(url) = base.join(media) {
                segments.push(Resource {
                    url: url.to_string(),
                    range: range.as_deref().and_then(parse_range),
                });
            }
        }
    } else if let Some(range) = index_range.as_deref().and_then(parse_range) {
        // A single-file representation. The `sidx` at this range says where the segments
        // are; until it is fetched there is nothing to enumerate.
        index = Some(Resource {
            url: base.to_string(),
            range: Some(range),
        });
    } else {
        // No segmenting information at all: the representation is one plain file.
        segments.push(Resource {
            url: base.to_string(),
            range: None,
        });
    }

    Some(Representation {
        id,
        content_type,
        mime,
        codecs,
        bandwidth,
        width: last(stack, |l| l.width),
        height: last(stack, |l| l.height),
        language: last(stack, |l| l.language.clone()),
        channels: last(stack, |l| l.channels),
        role_main: stack.iter().any(|l| l.role_main),
        init,
        segments,
        index,
        speculative,
    })
}

/// A segment list, and how much of its tail is guesswork.
struct Expanded {
    segments: Vec<Resource>,
    speculative: u32,
}

#[allow(clippy::too_many_arguments)]
fn expand_template(
    pattern: &str,
    template: &Template,
    timeline: &[TimelineEntry],
    id: &str,
    bandwidth: u64,
    period_duration: Option<f64>,
    base: &Url,
) -> Expanded {
    let mut out = Vec::new();
    let mut number = template.start_number;
    let timescale = if template.timescale > 0.0 {
        template.timescale
    } else {
        1.0
    };

    if !timeline.is_empty() {
        let mut time = template.presentation_time_offset;
        let end = period_duration.map(|d| (d * timescale) as u64);
        for entry in timeline {
            if let Some(t) = entry.t {
                time = t;
            }
            let repeats = if entry.r < 0 {
                // `r="-1"`: repeat to the end of the period. Without a period duration
                // there is nothing to repeat *to*, so the run is a single segment.
                match (end, entry.d) {
                    // A run of n segments is one segment plus n-1 repeats.
                    (Some(end), d) if d > 0 && end > time => ((end - time) / d) as i64 - 1,
                    _ => 0,
                }
            } else {
                entry.r
            };
            for _ in 0..=repeats {
                let expanded = expand(pattern, id, bandwidth, number, time);
                if let Ok(url) = base.join(&expanded) {
                    out.push(Resource {
                        url: url.to_string(),
                        range: None,
                    });
                }
                number += 1;
                time += entry.d;
            }
        }
        // A timeline states every segment explicitly. Nothing here is a guess.
        return Expanded {
            segments: out,
            speculative: 0,
        };
    }

    // `$Number$` with a nominal @duration.
    let (Some(duration), Some(total)) = (template.duration, period_duration) else {
        return Expanded {
            segments: out,
            speculative: 0,
        };
    };
    if duration <= 0.0 {
        return Expanded {
            segments: out,
            speculative: 0,
        };
    }
    let count = (total * timescale / duration).ceil() as u64;
    // Enough overshoot to cover an encoder whose segments run short, capped so a long VOD
    // does not generate hundreds of URLs nobody will ask for. Fetching stops at the first
    // speculative segment the server does not have.
    let speculative = (count / 8).clamp(2, 32);
    for i in 0..count + speculative {
        let expanded = expand(pattern, id, bandwidth, number + i, 0);
        if let Ok(url) = base.join(&expanded) {
            out.push(Resource {
                url: url.to_string(),
                range: None,
            });
        }
    }
    Expanded {
        segments: out,
        speculative: speculative as u32,
    }
}

/// `$Number%05d$`, `$Time$`, `$RepresentationID$`, `$Bandwidth$`, and `$$` for a literal.
fn expand(pattern: &str, id: &str, bandwidth: u64, number: u64, time: u64) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut rest = pattern;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        let Some(end) = rest.find('$') else {
            out.push('$');
            break;
        };
        let token = &rest[..end];
        rest = &rest[end + 1..];
        if token.is_empty() {
            out.push('$');
            continue;
        }
        let (name, format) = match token.split_once('%') {
            Some((name, format)) => (name, Some(format)),
            None => (token, None),
        };
        let value = match name {
            "RepresentationID" => {
                out.push_str(id);
                continue;
            }
            "Number" => number,
            "Time" => time,
            "Bandwidth" => bandwidth,
            _ => {
                out.push_str(token);
                continue;
            }
        };
        match format.and_then(pad_width) {
            Some(width) => out.push_str(&format!("{value:0width$}")),
            None => out.push_str(&value.to_string()),
        }
    }
    out.push_str(rest);
    out
}

/// `05d` → 5. Anything else is left unpadded rather than guessed at.
fn pad_width(format: &str) -> Option<usize> {
    let digits: String = format.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|width| *width > 0)
}

fn resolve_base(stack: &[Level], manifest: &Url) -> Url {
    let mut url = manifest.clone();
    for level in stack {
        if let Some(base) = &level.base {
            if let Ok(joined) = url.join(base) {
                url = joined;
            }
        }
    }
    url
}

fn last<T>(stack: &[Level], pick: impl Fn(&Level) -> Option<T>) -> Option<T> {
    stack.iter().rev().find_map(pick)
}

fn content_type(declared: Option<&str>, mime: Option<&str>, codecs: Option<&str>) -> Option<ContentType> {
    let hint = declared.or(mime).unwrap_or_default();
    if hint.starts_with("video") {
        return Some(ContentType::Video);
    }
    if hint.starts_with("audio") {
        return Some(ContentType::Audio);
    }
    if hint.starts_with("text") || hint.starts_with("application/ttml") {
        return Some(ContentType::Text);
    }
    // `application/mp4` is used for all three; the codec string is what disambiguates.
    let codecs = codecs.unwrap_or_default();
    if codecs.starts_with("avc") || codecs.starts_with("hvc") || codecs.starts_with("hev")
        || codecs.starts_with("vp0") || codecs.starts_with("vp9") || codecs.starts_with("av01")
    {
        return Some(ContentType::Video);
    }
    if codecs.starts_with("mp4a") || codecs.starts_with("opus") || codecs.starts_with("ac-3")
        || codecs.starts_with("ec-3") || codecs.starts_with("vorbis")
    {
        return Some(ContentType::Audio);
    }
    if codecs.starts_with("wvtt") || codecs.starts_with("stpp") {
        return Some(ContentType::Text);
    }
    None
}

fn read_mpd(e: &BytesStart<'_>, mpd: &mut Mpd) {
    mpd.dynamic = attr(e, "type").is_some_and(|t| t == "dynamic");
    mpd.duration = attr(e, "mediaPresentationDuration").and_then(|d| parse_duration(&d));
}

fn local_name(e: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(e.local_name().as_ref()).into_owned()
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let key = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        (key == name).then(|| String::from_utf8_lossy(&a.value).into_owned())
    })
}

/// `bytes=start-end` in DASH's own spelling: `start-end`, inclusive.
fn parse_range(text: &str) -> Option<(u64, u64)> {
    let (start, end) = text.trim().split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    (end >= start).then_some((start, end - start + 1))
}

/// ISO 8601 duration, the `PT1H2M3.5S` shape MPDs actually use.
pub fn parse_duration(text: &str) -> Option<f64> {
    let text = text.trim();
    let rest = text.strip_prefix('P')?;
    let (date, time) = match rest.split_once('T') {
        Some((date, time)) => (date, time),
        None => (rest, ""),
    };
    let mut seconds = 0.0f64;
    let mut number = String::new();
    for ch in date.chars() {
        match ch {
            'D' => {
                seconds += number.parse::<f64>().ok()? * 86_400.0;
                number.clear();
            }
            'Y' | 'M' | 'W' => {
                // Years and months are not a fixed number of seconds; a manifest that uses
                // them for a media duration is not one to guess at.
                return None;
            }
            c => number.push(c),
        }
    }
    number.clear();
    for ch in time.chars() {
        match ch {
            'H' => {
                seconds += number.parse::<f64>().ok()? * 3600.0;
                number.clear();
            }
            'M' => {
                seconds += number.parse::<f64>().ok()? * 60.0;
                number.clear();
            }
            'S' => {
                seconds += number.parse::<f64>().ok()?;
                number.clear();
            }
            c => number.push(c),
        }
    }
    Some(seconds)
}

/// The `sidx` box (ISO/IEC 14496-12 §8.16.3): the segment index of a single-file
/// representation. Returns one byte range per subsegment, starting immediately after the
/// box itself.
pub fn parse_sidx(bytes: &[u8], sidx_end: u64) -> Option<Vec<(u64, u64)>> {
    let mut at = 0usize;
    // Walk top-level boxes until `sidx`; a `styp` usually precedes it.
    loop {
        if at + 8 > bytes.len() {
            return None;
        }
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        if kind == b"sidx" {
            break;
        }
        if size < 8 {
            return None;
        }
        at += size;
    }

    let body = &bytes[at + 8..];
    let version = *body.first()?;
    let mut cursor = 4; // version + flags
    cursor += 4; // reference_ID
    // The timescale only matters for presentation timing, which the muxer recomputes.
    cursor += 4;
    let first_offset = if version == 0 {
        cursor += 4; // earliest_presentation_time
        let value = u32::from_be_bytes(body.get(cursor..cursor + 4)?.try_into().ok()?) as u64;
        cursor += 4;
        value
    } else {
        cursor += 8;
        let value = u64::from_be_bytes(body.get(cursor..cursor + 8)?.try_into().ok()?);
        cursor += 8;
        value
    };
    cursor += 2; // reserved
    let count = u16::from_be_bytes(body.get(cursor..cursor + 2)?.try_into().ok()?) as usize;
    cursor += 2;

    let mut offset = sidx_end + first_offset;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let word = u32::from_be_bytes(body.get(cursor..cursor + 4)?.try_into().ok()?);
        cursor += 12; // referenced_size+type, subsegment_duration, SAP word
        let size = (word & 0x7fff_ffff) as u64;
        out.push((offset, size));
        offset += size;
    }
    Some(out)
}

/// Reference-representation lookup used by the plan: the `id` a caller selected.
pub fn find<'a>(reps: &'a [Representation], id: &str) -> Option<&'a Representation> {
    reps.iter().find(|r| r.id == id)
}

/// Convenience for tests and the CLI: representations grouped by kind.
pub fn by_kind(reps: &[Representation]) -> HashMap<&'static str, Vec<&Representation>> {
    let mut out: HashMap<&'static str, Vec<&Representation>> = HashMap::new();
    for rep in reps {
        let key = match rep.content_type {
            ContentType::Video => "video",
            ContentType::Audio => "audio",
            ContentType::Text => "text",
        };
        out.entry(key).or_default().push(rep);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NUMBER_MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT0H1M0.00S">
  <ProgramInformation><Title>A Lecture</Title></ProgramInformation>
  <BaseURL>https://cdn.example.com/dash/</BaseURL>
  <Period duration="PT60S">
    <AdaptationSet mimeType="video/mp4" segmentAlignment="true">
      <SegmentTemplate initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg-$Number%05d$.m4s" startNumber="1" timescale="1000" duration="4000"/>
      <Representation id="v360" bandwidth="800000" width="640" height="360" codecs="avc1.4d401e"/>
      <Representation id="v1080" bandwidth="5000000" width="1920" height="1080" codecs="avc1.640028"/>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4" lang="en">
      <SegmentTemplate initialization="a/init.mp4" media="a/seg-$Number$.m4s" startNumber="1" timescale="1000" duration="4000"/>
      <Representation id="a128" bandwidth="128000" codecs="mp4a.40.2">
        <AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="2"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn a_number_template_expands_with_padding_and_inheritance() {
        let mpd = parse(NUMBER_MPD, "https://example.com/x/manifest.mpd")
            .unwrap()
            .unwrap();
        assert!(!mpd.dynamic);
        assert_eq!(mpd.title.as_deref(), Some("A Lecture"));
        assert_eq!(mpd.duration, Some(60.0));
        let reps = ladder(&mpd);
        assert_eq!(reps.len(), 3);

        let v1080 = find(&reps, "v1080").unwrap();
        assert_eq!(v1080.content_type, ContentType::Video);
        assert_eq!(v1080.height, Some(1080));
        assert_eq!(
            v1080.init.as_ref().unwrap().url,
            "https://cdn.example.com/dash/v1080/init.mp4"
        );
        // 60 s at 4 s segments is fifteen the manifest promises, plus a short tail that
        // exists to be probed: a nominal @duration is an estimate, and encoders undershoot.
        assert_eq!(v1080.speculative, 2);
        assert_eq!(v1080.segments.len(), 15 + v1080.speculative as usize);
        assert_eq!(
            v1080.segments[0].url,
            "https://cdn.example.com/dash/v1080/seg-00001.m4s"
        );
        assert_eq!(
            v1080.segments[14].url,
            "https://cdn.example.com/dash/v1080/seg-00015.m4s"
        );

        let audio = find(&reps, "a128").unwrap();
        assert_eq!(audio.content_type, ContentType::Audio);
        assert_eq!(audio.language.as_deref(), Some("en"));
        assert_eq!(audio.channels, Some(2));
        assert_eq!(audio.segments[0].url, "https://cdn.example.com/dash/a/seg-1.m4s");
    }

    #[test]
    fn a_segment_timeline_drives_time_and_repeats() {
        let text = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT12S">
  <Period duration="PT12S">
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <Representation id="v" bandwidth="1000" codecs="avc1.4d401e">
        <SegmentTemplate media="v/$Time$.m4s" initialization="v/init.mp4" timescale="1000">
          <SegmentTimeline>
            <S t="0" d="2000" r="2"/>
            <S d="3000"/>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = parse(text, "https://example.com/v/m.mpd").unwrap().unwrap();
        let reps = ladder(&mpd);
        let urls: Vec<_> = reps[0].segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://example.com/v/v/0.m4s",
                "https://example.com/v/v/2000.m4s",
                "https://example.com/v/v/4000.m4s",
                "https://example.com/v/v/6000.m4s",
            ],
            "r=2 means three segments, and the next S continues where it stopped"
        );
    }

    #[test]
    fn an_open_ended_repeat_stops_at_the_end_of_the_period() {
        let text = r#"<MPD type="static" mediaPresentationDuration="PT10S">
  <Period duration="PT10S">
    <AdaptationSet contentType="video">
      <Representation id="v" bandwidth="1" codecs="avc1.1">
        <SegmentTemplate media="s$Number$.m4s" timescale="1">
          <SegmentTimeline><S t="0" d="2" r="-1"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let mpd = parse(text, "https://example.com/m.mpd").unwrap().unwrap();
        assert_eq!(ladder(&mpd)[0].segments.len(), 5);
    }

    #[test]
    fn content_protection_ends_the_parse_with_a_named_refusal() {
        let text = r#"<MPD type="static"><Period><AdaptationSet contentType="video">
          <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
          <Representation id="v" bandwidth="1" codecs="avc1.1"/>
        </AdaptationSet></Period></MPD>"#;
        let refusal = parse(text, "https://example.com/m.mpd").unwrap().unwrap_err();
        assert_eq!(refusal.system, "Widevine");
    }

    #[test]
    fn iso_durations_parse_the_shapes_manifests_use() {
        assert_eq!(parse_duration("PT1H2M3.5S"), Some(3723.5));
        assert_eq!(parse_duration("PT0H1M0.00S"), Some(60.0));
        assert_eq!(parse_duration("PT30S"), Some(30.0));
        assert_eq!(parse_duration("P1DT2H"), Some(93_600.0));
        assert_eq!(parse_duration("P1M"), None, "a month is not a duration");
    }

    #[test]
    fn a_sidx_becomes_a_list_of_byte_ranges() {
        // version 0, one reference of 1000 bytes, first_offset 0.
        let mut body = Vec::new();
        body.extend_from_slice(&[0, 0, 0, 0]); // version + flags
        body.extend_from_slice(&1u32.to_be_bytes()); // reference_ID
        body.extend_from_slice(&90_000u32.to_be_bytes()); // timescale
        body.extend_from_slice(&0u32.to_be_bytes()); // earliest_presentation_time
        body.extend_from_slice(&0u32.to_be_bytes()); // first_offset
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        body.extend_from_slice(&2u16.to_be_bytes()); // reference_count
        for size in [1000u32, 2000] {
            body.extend_from_slice(&size.to_be_bytes());
            body.extend_from_slice(&45_000u32.to_be_bytes());
            body.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let mut boxed = Vec::new();
        boxed.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        boxed.extend_from_slice(b"sidx");
        boxed.extend_from_slice(&body);

        let ranges = parse_sidx(&boxed, 500).unwrap();
        assert_eq!(ranges, [(500, 1000), (1500, 2000)]);
    }

    #[test]
    fn a_segment_list_is_taken_literally() {
        let text = r#"<MPD type="static"><Period duration="PT8S"><AdaptationSet contentType="video">
          <Representation id="v" bandwidth="1" codecs="avc1.1">
            <SegmentList duration="4">
              <Initialization sourceURL="init.mp4"/>
              <SegmentURL media="a.m4s"/>
              <SegmentURL media="b.m4s" mediaRange="0-99"/>
            </SegmentList>
          </Representation>
        </AdaptationSet></Period></MPD>"#;
        let mpd = parse(text, "https://example.com/d/m.mpd").unwrap().unwrap();
        let rep = &ladder(&mpd)[0];
        assert_eq!(rep.init.as_ref().unwrap().url, "https://example.com/d/init.mp4");
        assert_eq!(rep.segments.len(), 2);
        assert_eq!(rep.segments[1].range, Some((0, 100)));
    }
}
