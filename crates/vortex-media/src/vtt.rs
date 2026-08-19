//! Stitching segmented WebVTT into one file.
//!
//! Subtitle renditions are segmented like everything else in HLS, but unlike media
//! segments WebVTT segments cannot simply be concatenated: each one repeats the `WEBVTT`
//! header, and each carries its own `X-TIMESTAMP-MAP` saying where its local clock sits on
//! the media timeline. Byte-appending them produces a file that players either reject
//! outright or render with every cue at the wrong time — which looks like a Vortex bug and
//! is in fact a WebVTT rule.
//!
//! So: keep one header, drop the rest, and shift each segment's cues by the offset its
//! timestamp map declares.

/// Converts one fetched segment into the bytes that belong in the output file.
///
/// `first` writes the single `WEBVTT` header the file is allowed to have.
pub fn stitch(segment: &[u8], first: bool) -> Vec<u8> {
    let text = String::from_utf8_lossy(segment);
    let text = text.trim_start_matches('\u{feff}');
    let (header, body) = split_header(text);
    let offset = header.and_then(timestamp_map).unwrap_or(0.0);

    let mut out = String::with_capacity(segment.len() + 16);
    if first {
        out.push_str("WEBVTT\n\n");
    }
    for line in body.lines() {
        match shift_cue(line, offset) {
            Some(shifted) => out.push_str(&shifted),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    if !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.into_bytes()
}

/// Splits the header block — `WEBVTT` plus any `KEY:VALUE` lines — from the cues.
fn split_header(text: &str) -> (Option<&str>, &str) {
    let Some(rest) = text.strip_prefix("WEBVTT") else {
        // Not a segment with a header: a continuation, or a plain `.vtt` body.
        return (None, text);
    };
    match rest.split_once("\n\n").or_else(|| rest.split_once("\r\n\r\n")) {
        Some((header, body)) => (Some(header), body),
        // A header and nothing else. Legal, and it happens on gap segments.
        None => (Some(rest), ""),
    }
}

/// `X-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000` — the seconds to add to every cue.
fn timestamp_map(header: &str) -> Option<f64> {
    let line = header
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("X-TIMESTAMP-MAP"))?;
    let value = line.split_once('=')?.1;

    let mut mpegts = None;
    let mut local = None;
    for field in value.split(',') {
        let (key, raw) = field.trim().split_once(':')?;
        match key.trim() {
            "MPEGTS" => mpegts = raw.trim().parse::<f64>().ok(),
            "LOCAL" => local = parse_timestamp(raw.trim()),
            _ => {}
        }
    }
    // The MPEG-2 transport stream clock runs at 90 kHz. This is the one constant in HLS
    // that is never spelled out in the playlist.
    let offset = mpegts? / 90_000.0 - local?;
    Some(offset)
}

/// `00:00:12.500 --> 00:00:15.000 line:0%` with every timestamp moved by `offset`.
fn shift_cue(line: &str, offset: f64) -> Option<String> {
    let (start, rest) = line.split_once("-->")?;
    let start_time = parse_timestamp(start.trim())?;
    let mut tail = rest.trim_start().splitn(2, char::is_whitespace);
    let end_time = parse_timestamp(tail.next()?.trim())?;
    let settings = tail.next().unwrap_or("");

    let mut out = format!(
        "{} --> {}",
        format_timestamp(start_time + offset),
        format_timestamp(end_time + offset)
    );
    if !settings.is_empty() {
        out.push(' ');
        out.push_str(settings.trim_end());
    }
    Some(out)
}

/// `hh:mm:ss.mmm` or `mm:ss.mmm`, which WebVTT allows interchangeably.
fn parse_timestamp(text: &str) -> Option<f64> {
    let mut seconds = 0.0f64;
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    for part in &parts[..parts.len() - 1] {
        seconds = seconds * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    // Only the last field carries fractional seconds, and it may use a comma in files
    // that have been round-tripped through SRT.
    let last = parts.last()?.replace(',', ".");
    Some(seconds * 60.0 + last.trim().parse::<f64>().ok()?)
}

fn format_timestamp(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let whole = seconds as u64;
    let millis = ((seconds - whole as f64) * 1000.0).round() as u64;
    // Rounding 1.9996 up must not produce `:01.1000`.
    let (whole, millis) = if millis >= 1000 {
        (whole + 1, millis - 1000)
    } else {
        (whole, millis)
    };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        whole / 3600,
        (whole % 3600) / 60,
        whole % 60,
        millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEGMENT: &str = "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000\n\n\
                           00:00:01.000 --> 00:00:03.000\nHello\n";

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn the_first_segment_keeps_one_header_and_the_rest_keep_none() {
        let first = text(stitch(SEGMENT.as_bytes(), true));
        assert!(first.starts_with("WEBVTT\n\n"), "{first:?}");
        assert_eq!(first.matches("WEBVTT").count(), 1);

        let second = text(stitch(SEGMENT.as_bytes(), false));
        assert!(
            !second.contains("WEBVTT"),
            "a repeated header makes the whole file unreadable: {second:?}"
        );
        assert!(!second.contains("X-TIMESTAMP-MAP"), "{second:?}");
    }

    #[test]
    fn cues_are_moved_onto_the_media_timeline() {
        // MPEGTS 900000 is ten seconds at 90 kHz, and LOCAL is zero, so every cue moves
        // ten seconds later. Getting this wrong shows subtitles at the wrong moment, which
        // reads as a broken download rather than as a WebVTT detail.
        let out = text(stitch(SEGMENT.as_bytes(), true));
        assert!(out.contains("00:00:11.000 --> 00:00:13.000"), "{out}");
    }

    #[test]
    fn a_local_offset_is_subtracted_not_added() {
        let segment = "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:04.000\n\n\
                       00:00:05.000 --> 00:00:06.000\nLater\n";
        let out = text(stitch(segment.as_bytes(), false));
        assert!(out.contains("00:00:11.000 --> 00:00:12.000"), "{out}");
    }

    #[test]
    fn a_segment_without_a_timestamp_map_is_left_where_it_is() {
        let segment = "WEBVTT\n\n00:00:02.000 --> 00:00:04.000\nPlain\n";
        let out = text(stitch(segment.as_bytes(), true));
        assert!(out.contains("00:00:02.000 --> 00:00:04.000"), "{out}");
    }

    #[test]
    fn cue_settings_and_identifiers_survive() {
        let segment = "WEBVTT\n\ncue-7\n00:00:02.000 --> 00:00:04.000 line:90% align:center\nText\n";
        let out = text(stitch(segment.as_bytes(), true));
        assert!(out.contains("cue-7"), "{out}");
        assert!(
            out.contains("00:00:02.000 --> 00:00:04.000 line:90% align:center"),
            "{out}"
        );
        assert!(out.contains("Text"), "{out}");
    }

    #[test]
    fn short_and_comma_timestamps_are_understood() {
        assert_eq!(parse_timestamp("00:01.500"), Some(1.5));
        assert_eq!(parse_timestamp("01:02:03.250"), Some(3723.25));
        assert_eq!(parse_timestamp("00:00:01,250"), Some(1.25));
        assert_eq!(parse_timestamp("nonsense"), None);
        assert_eq!(format_timestamp(3723.25), "01:02:03.250");
        assert_eq!(format_timestamp(1.9996), "00:00:02.000");
    }

    #[test]
    fn an_empty_segment_contributes_nothing_but_does_not_break_the_file() {
        let out = text(stitch(b"WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n", false));
        assert!(out.trim().is_empty(), "{out:?}");
    }
}
