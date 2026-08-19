//! Naming and path safety (01 §Security, 04 §8).
//!
//! Priority order, first hit wins:
//!
//! 1. `Content-Disposition: filename*` (RFC 5987, UTF-8 aware)
//! 2. a manifest title
//! 3. the page `<title>`, with site-name suffixes stripped
//! 4. the URL path basename
//!
//! Then sanitize, dedupe with ` (2)`, and cap the total path so the `.vxpart` temp path
//! never exceeds `MAX_PATH` *before* the final file does.

use percent_encoding::percent_decode_str;
use std::path::{Path, PathBuf};
use vortex_proto::Category;

/// Reserved Windows device names. These are refused on every platform: a file called
/// `CON` is a landmine the moment the download folder is shared or synced.
const RESERVED_DEVICES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Windows `MAX_PATH` is 260; 240 leaves room for ` (2)` and the `.vxpart.meta` suffix.
const MAX_PATH_CHARS: usize = 240;
const MAX_COMPONENT_BYTES: usize = 255;
/// `.vxpart.meta` is the longest suffix the engine appends.
const SUFFIX_ALLOWANCE: usize = 13;

/// Strips separators, control characters and Windows-illegal characters; refuses reserved
/// device names; caps the component at 255 UTF-8 bytes; strips trailing dots and spaces
/// (which Windows silently drops, turning `a.txt.` into a name that never matches).
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {}
            c => out.push(c),
        }
    }
    let mut trimmed = out.trim().trim_end_matches(['.', ' ']).to_owned();
    if trimmed.is_empty() {
        trimmed = "download".to_owned();
    }

    let stem_end = trimmed.rfind('.').unwrap_or(trimmed.len());
    if RESERVED_DEVICES
        .iter()
        .any(|d| trimmed[..stem_end].eq_ignore_ascii_case(d))
    {
        trimmed.insert(0, '_');
    }

    truncate_bytes(&trimmed, MAX_COMPONENT_BYTES)
}

/// Truncates on a character boundary, keeping the extension — a name that loses `.iso` is
/// worse than a name that loses a few words.
fn truncate_bytes(name: &str, limit: usize) -> String {
    if name.len() <= limit {
        return name.to_owned();
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.len() <= 16)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let room = limit.saturating_sub(ext.len());
    let mut stem = &name[..name.len().min(room)];
    while !name.is_char_boundary(stem.len()) {
        stem = &name[..stem.len() - 1];
    }
    format!("{}{}", stem.trim_end(), ext)
}

/// Derives a filename from `Content-Disposition`, falling back to the URL basename.
pub fn from_disposition(disposition: Option<&str>, url: &str) -> Option<String> {
    if let Some(value) = disposition {
        if let Some(name) = parse_disposition(value) {
            return Some(sanitize(&name));
        }
    }
    from_url(url)
}

/// RFC 6266/5987. `filename*=UTF-8''a%20b.iso` wins over `filename="a b.iso"`.
pub fn parse_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    for part in split_params(value) {
        let (key, raw) = part.split_once('=')?;
        let key = key.trim().to_ascii_lowercase();
        let raw = raw.trim();
        if key == "filename*" {
            // charset'language'percent-encoded
            let mut it = raw.splitn(3, '\'');
            let charset = it.next().unwrap_or("").to_ascii_lowercase();
            let _lang = it.next();
            if let Some(encoded) = it.next() {
                let decoded = percent_decode_str(encoded);
                let text = if charset == "utf-8" || charset.is_empty() {
                    decoded.decode_utf8_lossy().into_owned()
                } else {
                    // latin-1 is the only other charset RFC 5987 allows.
                    decoded.map(|b| b as char).collect()
                };
                if !text.trim().is_empty() {
                    return Some(text);
                }
            }
        } else if key == "filename" {
            let text = raw.trim_matches('"');
            if !text.trim().is_empty() {
                plain = Some(text.to_owned());
            }
        }
    }
    plain
}

/// Splits header parameters on semicolons that are not inside quotes.
fn split_params(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in value.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            ';' if !quoted => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    parts.push(current);
    parts.into_iter().filter(|p| p.contains('=')).collect()
}

pub fn from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.rfind(|s| !s.is_empty())?;
    let decoded = percent_decode_str(last).decode_utf8_lossy().into_owned();
    if decoded.trim().is_empty() {
        None
    } else {
        Some(sanitize(&decoded))
    }
}

/// Page `<title>` with the site-name suffix stripped — "Session 3 — Distributed Consensus"
/// rather than "Session 3 — Distributed Consensus | ConfName 2026".
pub fn from_page_title(title: &str) -> String {
    let cleaned = title
        .split(['|', '·'])
        .next()
        .unwrap_or(title)
        .trim()
        .to_owned();
    sanitize(if cleaned.is_empty() { title } else { &cleaned })
}

/// Guessed from extension first, then MIME. The sidebar's `kind` grouping depends on this
/// being boringly predictable.
pub fn categorize(filename: &str, mime: Option<&str>) -> Category {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "ts" | "flv" | "wmv" => {
            return Category::Video
        }
        "mp3" | "m4a" | "flac" | "wav" | "aac" | "ogg" | "opus" => return Category::Audio,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" | "dmg" => {
            return Category::Archives
        }
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "csv" => {
            return Category::Documents
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" | "heic" => {
            return Category::Images
        }
        "exe" | "msi" | "deb" | "rpm" | "apk" | "appimage" | "pkg" => return Category::Programs,
        _ => {}
    }
    match mime.map(|m| m.split(';').next().unwrap_or("").trim().to_ascii_lowercase()) {
        Some(m) if m.starts_with("video/") => Category::Video,
        Some(m) if m.starts_with("audio/") => Category::Audio,
        Some(m) if m.starts_with("image/") => Category::Images,
        Some(m) if m == "application/pdf" || m.starts_with("text/") => Category::Documents,
        Some(m) if m.contains("zip") || m.contains("tar") || m.contains("compressed") => {
            Category::Archives
        }
        _ => Category::Other,
    }
}

/// Picks a free path, appending ` (2)`, ` (3)`… and keeping the whole path inside the
/// platform limit. Reserves room for the `.vxpart.meta` suffix so the temp path never
/// exceeds `MAX_PATH` before the final one does.
pub fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let filename = fit_path(dir, &sanitize(filename));
    let mut candidate = dir.join(&filename);
    if !exists_any(&candidate) {
        return candidate;
    }
    let stem = Path::new(&filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_owned();
    let ext = Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();

    for n in 2..10_000 {
        let name = fit_path(dir, &format!("{stem} ({n}){ext}"));
        candidate = dir.join(name);
        if !exists_any(&candidate) {
            return candidate;
        }
    }
    candidate
}

/// A name is taken if the final file exists *or* a partial for it does — otherwise two
/// jobs would fight over one `.vxpart`.
fn exists_any(path: &Path) -> bool {
    if path.exists() {
        return true;
    }
    let mut part = path.as_os_str().to_os_string();
    part.push(".vxpart");
    Path::new(&part).exists()
}

fn fit_path(dir: &Path, filename: &str) -> String {
    let room = MAX_PATH_CHARS
        .saturating_sub(dir.as_os_str().len() + 1 + SUFFIX_ALLOWANCE)
        .max(24);
    truncate_bytes(filename, room)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_control_characters_never_survive() {
        assert_eq!(sanitize("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize("a\u{0}b.txt"), "ab.txt");
        assert_eq!(sanitize("report:2025?.pdf"), "report_2025_.pdf");
    }

    #[test]
    fn windows_device_names_are_defused() {
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul.txt"), "_nul.txt");
        assert_eq!(sanitize("console.log"), "console.log");
    }

    #[test]
    fn trailing_dots_and_spaces_are_stripped() {
        assert_eq!(sanitize("report.pdf.  "), "report.pdf");
        assert_eq!(sanitize("   "), "download");
    }

    #[test]
    fn a_long_name_keeps_its_extension() {
        let name = format!("{}.iso", "x".repeat(400));
        let out = sanitize(&name);
        assert!(out.len() <= MAX_COMPONENT_BYTES);
        assert!(out.ends_with(".iso"));
    }

    #[test]
    fn rfc_5987_filenames_win_over_the_plain_one() {
        let value = "attachment; filename=\"fallback.iso\"; filename*=UTF-8''Ubuntu%2024.04%20%E2%80%94%20desktop.iso";
        assert_eq!(
            parse_disposition(value).as_deref(),
            Some("Ubuntu 24.04 — desktop.iso")
        );
    }

    #[test]
    fn a_semicolon_inside_quotes_does_not_split_the_header() {
        let value = "attachment; filename=\"a;b.iso\"";
        assert_eq!(parse_disposition(value).as_deref(), Some("a;b.iso"));
    }

    #[test]
    fn url_basenames_are_decoded() {
        assert_eq!(
            from_url("https://x.example/dl/Ubuntu%2024.04.iso?token=1").as_deref(),
            Some("Ubuntu 24.04.iso")
        );
        assert_eq!(from_url("https://x.example/"), None);
    }

    #[test]
    fn page_titles_lose_the_site_suffix() {
        assert_eq!(
            from_page_title("Session 3 — Distributed Consensus | ConfName 2026"),
            "Session 3 — Distributed Consensus"
        );
    }

    #[test]
    fn categories_come_from_the_extension_then_the_mime_type() {
        assert_eq!(categorize("a.iso", None), Category::Archives);
        assert_eq!(categorize("a.mp4", None), Category::Video);
        assert_eq!(categorize("a", Some("video/mp4")), Category::Video);
        assert_eq!(categorize("a", Some("application/pdf")), Category::Documents);
        assert_eq!(categorize("a", None), Category::Other);
    }

    #[test]
    fn duplicate_names_get_a_numbered_sibling() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.iso"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.iso").file_name().unwrap(),
            "a (2).iso"
        );
    }

    #[test]
    fn an_existing_partial_also_claims_the_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.iso.vxpart"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.iso").file_name().unwrap(),
            "a (2).iso"
        );
    }
}
