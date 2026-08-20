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
//! **Credentials never appear in argv, and they do reach the extractor.** Those are two
//! separate rules and the first one used to be enforced by dropping the second.
//!
//! Running the extraction unauthenticated was the original design, on the reasoning that a
//! site needing the session is a site whose manifest our own parser reads. YouTube ended
//! that: every player client now answers `Sign in to confirm you're not a bot` to a request
//! with no session on it, so an unauthenticated extraction cannot answer for the single
//! largest video site on the web — which is most of what channel 5 exists for.
//!
//! So the page's own cookies go across, in the one shape that keeps the first rule intact:
//! a [`CookieJar`] written to a temporary file for the life of the child and deleted when
//! it exits. Never `--add-header Cookie:`, because argv is readable by every process on the
//! machine. The jar holds only what the browser itself would send to *this* page's host, so
//! an extraction cannot carry one site's session to another, and nothing is written where
//! `vortex.db`, a log line or a `.vxpart.meta` could keep it (01 §Security boundaries).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vortex_proto::{MediaCandidate, MediaKind, MediaTrack, MediaVariant, RequestEnvelope};

/// Headers worth replaying to an extractor. Everything that identifies a *user* is absent —
/// `Cookie` included, because this list becomes argv. It travels in the jar instead.
const FORWARDED: [&str; 2] = ["user-agent", "referer"];

/// How long a written cookie is claimed to last.
///
/// A Netscape jar has no way to say "session cookie": Python's reader treats an expiry it
/// has already passed as a cookie to drop, so `0` would silently discard exactly the
/// session cookies that make the extraction work. The file outlives the extraction by
/// nothing at all, so the number only has to be in the future.
const JAR_LIFETIME: Duration = Duration::from_secs(365 * 24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct YtDlp(PathBuf);

impl YtDlp {
    /// Bundled beside the daemon first — extractors break weekly and ship on their own
    /// channel — then `VORTEX_YTDLP`, then `PATH`.
    pub fn find() -> Option<Self> {
        if let Some(beside) = beside_daemon("yt-dlp") {
            return Some(Self(beside));
        }
        if let Some(configured) = std::env::var_os("VORTEX_YTDLP").map(PathBuf::from) {
            if configured.is_file() {
                return Some(Self(configured));
            }
        }
        on_path("yt-dlp").map(Self)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// The runtimes yt-dlp will drive, as `(the name it knows, the file on disk)`.
///
/// The two are not always the same word — QuickJS ships as `qjs` — and yt-dlp needs the
/// name to decide which dialect to speak, so neither half can be derived from the other.
///
/// In yt-dlp's order of preference, except that `bun` is absent: it deprecated every
/// version after 1.3.14, so offering one would be choosing a runtime that is refused on
/// arrival over one that works. QuickJS is what Vortex bundles — 2 MB against Deno's
/// hundred-odd, for the same job — and a machine that already has Deno or Node gets the
/// faster engine it already paid for.
const JS_RUNTIMES: [(&str, &str); 3] = [("deno", "deno"), ("node", "node"), ("quickjs", "qjs")];

/// A JavaScript engine for the extractor to borrow.
///
/// YouTube's player challenge is JavaScript, and yt-dlp stopped shipping its own
/// interpreter for it: without a runtime the extraction is on a deprecated path that says
/// out loud that *some formats may be missing*, which shows up here as a ladder with rungs
/// absent for no visible reason. yt-dlp only looks for `deno` by default, so a machine with
/// Node on it — which is most machines — silently takes the degraded path unless it is told.
///
/// Not fatal when there is none. The extractor still answers for the many sites that need
/// no JavaScript at all, and a missing runtime is a worse answer, not no answer.
#[derive(Debug, Clone)]
pub struct JsRuntime {
    name: &'static str,
    path: PathBuf,
}

impl JsRuntime {
    /// Bundled beside the daemon first, then `VORTEX_JS_RUNTIME`, then `PATH` — the same
    /// order as the extractor itself, for the same reason.
    pub fn find() -> Option<Self> {
        // The bundled one first, whichever it is, before anything the machine happens to
        // have: it is the build whose version we know, and yt-dlp is slow to the point of
        // minutes on a QuickJS older than 2025-04-26.
        for (name, file) in JS_RUNTIMES {
            if let Some(path) = beside_daemon(file) {
                return Some(Self { name, path });
            }
        }
        if let Some(configured) = std::env::var_os("VORTEX_JS_RUNTIME").map(PathBuf::from) {
            // The name is how yt-dlp decides which dialect to speak, so a path whose
            // filename names none of them is not usable however real the binary is.
            if let Some(name) = runtime_named(&configured) {
                if configured.is_file() {
                    return Some(Self { name, path: configured });
                }
            }
        }
        JS_RUNTIMES
            .into_iter()
            .find_map(|(name, file)| on_path(file).map(|path| Self { name, path }))
    }

    /// yt-dlp's `RUNTIME[:PATH]`. The path is always given: the runtime was resolved here,
    /// and letting yt-dlp search again could find a different one.
    fn argument(&self) -> String {
        format!("{}:{}", self.name, self.path.display())
    }
}

/// Which runtime a path names, by its filename. `…/qjs.exe` is QuickJS.
fn runtime_named(path: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    JS_RUNTIMES
        .into_iter()
        .find_map(|(name, file)| (file == stem || name == stem).then_some(name))
}

/// A program shipped in the same directory as the calling executable.
fn beside_daemon(name: &str) -> Option<PathBuf> {
    let candidate = std::env::current_exe().ok()?.parent()?.join(executable(name));
    candidate.is_file().then_some(candidate)
}

/// A bare name looked up on `PATH`. `Command` would do this at spawn time, but then a
/// missing program is a spawn failure halfway through rather than something to plan around.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(executable(name)))
        .find(|candidate| candidate.is_file())
}

fn executable(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// The page's cookies, in the only file format yt-dlp reads, for the life of one child.
///
/// A [`tempfile::TempPath`] rather than an open `NamedTempFile`, and the difference is not
/// cosmetic: `--cookies` makes yt-dlp *write the jar back* as it exits, and on Windows a
/// handle we were still holding is a share violation at the very end of an otherwise good
/// extraction. So the handle is closed the moment the bytes are down, and what is kept is
/// the promise to delete the path.
struct CookieJar {
    path: tempfile::TempPath,
    /// How many cookies went across. The names are not logged and the values never are,
    /// but "the jar held eleven" and "the jar held none" are different bug reports.
    count: usize,
}

impl CookieJar {
    /// Writes a `Cookie:` header out as a Netscape jar. `None` when there is nothing worth
    /// writing, which is the ordinary case and not a failure.
    fn write(header: &str, url: &str) -> Option<Self> {
        let body = render(header, url)?;
        // The system temp directory is per-user on Windows, and `tempfile` creates with
        // `0600` on Unix, so the file is never readable by another account.
        let mut file = tempfile::Builder::new()
            .prefix("vortex-cookies-")
            .suffix(".txt")
            .tempfile()
            .map_err(|e| tracing::warn!("no cookie jar for the extractor: {e}"))
            .ok()?;
        file.write_all(body.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|e| tracing::warn!("could not write the cookie jar: {e}"))
            .ok()?;
        Some(Self {
            path: file.into_temp_path(),
            count: body.lines().count().saturating_sub(1), // less the magic line
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// One `Cookie: a=1; b=2` header as a Netscape cookie file. Pure, so the format can be
/// asserted on without a filesystem or an extractor.
///
/// **One line per cookie, under exactly one domain.** An extractor does not necessarily
/// talk to the exact host the tab is on — yt-dlp's YouTube client reaches `www.youtube.com`
/// from an `m.youtube.com` page and back again — so a `www.` or `m.` host is widened by
/// that one prefix, and only that one, so the widening can never reach past the site the
/// cookies already belong to.
///
/// It is widened by **rewriting** the domain rather than by adding a second copy, and that
/// distinction is the whole of a bug this used to have. `.www.youtube.com` and
/// `.youtube.com` are two separate cookies to a cookie jar and both match a request to
/// `www.youtube.com`, so writing both sent every cookie twice. On a signed-out page that is
/// a duplicated `YSC` and nothing anywhere notices; on a signed-in one it is two
/// contradictory copies each of `SID`, `SAPISID` and `__Secure-3PSID`, and YouTube answers
/// a request carrying those with a page that has no `ytcfg` in it at all — which arrives
/// here, three layers later, as `KeyError('INNERTUBE_CONTEXT')`.
///
/// The leading-dot form already matches every subdomain, so one line was always enough.
fn render(header: &str, url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    let domain = format!(
        ".{}",
        host.strip_prefix("www.")
            .or_else(|| host.strip_prefix("m."))
            .filter(|parent| parent.contains('.'))
            .unwrap_or(&host)
    );

    let expiry = (SystemTime::now() + JAR_LIFETIME)
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();

    // The magic first line is load-bearing: a jar without it is refused outright rather
    // than read as an empty one.
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    let mut written = 0usize;
    for pair in header.split(';') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        // The format is tab-separated and line-oriented, so a value carrying either would
        // not round-trip. Dropping one cookie beats writing a file that reads as garbage.
        if name.is_empty() || [name, value].iter().any(|f| f.contains(['\t', '\n', '\r'])) {
            continue;
        }
        // domain, include-subdomains, path, secure, expiry, name, value.
        out.push_str(&format!("{domain}\tTRUE\t/\tTRUE\t{expiry}\t{name}\t{value}\n"));
        written += 1;
    }
    (written > 0).then_some(out)
}

/// What an extraction found.
///
/// The two are not variations on one answer, they are different jobs. A ladder of finished
/// URLs is something the scheduler can start on immediately. A manifest is something our
/// own parsers should read — they build the real ladder, with its audio renditions, its
/// subtitle tracks and its byte ranges, none of which survive being flattened into an
/// extractor's list of formats.
#[derive(Debug, Clone)]
pub enum Extracted {
    /// Finished URLs. One file per track, fetched directly.
    Ladder(Box<MediaCandidate>),
    /// Where the stream is described. Handed straight back to [`crate::plan::inspect`].
    Manifest(String),
}

/// Asks yt-dlp what is on a page. Never asks it to fetch anything.
pub async fn extract(
    tool: &YtDlp,
    envelope: &RequestEnvelope,
    preferred_height: u32,
) -> Result<Extracted, String> {
    let url = envelope.effective_url();
    let mut command = tokio::process::Command::new(tool.path());
    command
        .arg("--dump-single-json")
        .arg("--no-playlist")
        .arg("--skip-download");
    // Warnings are deliberately left on. `--no-warnings` used to be here, and it hid the
    // one line that explained a whole class of failure — "no supported JavaScript runtime
    // could be found" — behind an error message about signing in. Nothing reads stdout for
    // anything but the JSON, so the cost of keeping them is a debug line.
    match JsRuntime::find() {
        Some(runtime) => {
            command.arg("--js-runtimes").arg(runtime.argument());
        }
        None => tracing::debug!("no JavaScript runtime for the extractor to borrow"),
    }
    for (name, value) in &envelope.headers {
        if FORWARDED.iter().any(|f| name.eq_ignore_ascii_case(f)) {
            command.arg("--add-header").arg(format!("{name}:{value}"));
        }
    }
    // Held in scope until the child has exited: dropping the path deletes the file.
    let jar = envelope
        .cookies
        .as_deref()
        .and_then(|header| CookieJar::write(header, url));
    match &jar {
        Some(jar) => {
            command.arg("--cookies").arg(jar.path());
        }
        None => {
            // Not an error — plenty of pages need no session — but the one failure this
            // explains ("sign in to confirm you're not a bot") is otherwise unreadable.
            tracing::debug!("extraction has no cookies for this page");
        }
    }
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = crate::child::windowless(&mut command)
        .output()
        .await
        .map_err(|e| format!("yt-dlp would not start: {e}"))?;
    let carried = jar.as_ref().map_or(0, |jar| jar.count);
    drop(jar);

    let said = String::from_utf8_lossy(&output.stderr);
    let failed = !output.status.success();
    // Reported on the way *out*, not only on the way down: a warning on a run that produced
    // a ladder is how "the ladder is missing its top three rungs" gets explained.
    //
    // A failed run says it at `warn`, and the level is not a matter of taste. The installed
    // daemon logs at info, so `debug!` is legible in the one configuration that is being
    // watched by hand and silent in the one the user is running when they ask why nothing
    // appeared. This whole block existed and said nothing, twice, for that reason.
    if failed {
        tracing::warn!(url, cookies = carried, "extraction failed");
    }
    for line in said.lines().filter(|line| !line.trim().is_empty()) {
        if failed {
            tracing::warn!("yt-dlp: {line}");
        } else {
            tracing::debug!("yt-dlp: {line}");
        }
    }
    if failed {
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!("Vortex couldn't find a video on that page. {last}"));
    }
    parse(&String::from_utf8_lossy(&output.stdout), url, preferred_height)
}

/// Turns one `--dump-single-json` object into something the pipeline can act on. Pure, so
/// the shape of yt-dlp's output can be asserted on without yt-dlp installed.
///
/// The formats are **partitioned by protocol before anything else**, because a large site
/// publishes both families at once — YouTube currently answers with thirty-odd progressive
/// URLs *and* a dozen HLS playlists for the same video — and they are not interchangeable
/// rungs of one ladder. A `720p` that is a finished MP4 and a `720p` that is a playlist
/// need different machinery to fetch, and a ladder that mixes them silently hands one to
/// the other's code path.
///
/// Direct URLs win when there are any, because they carry exact sizes and a complete
/// resolution ladder. Everything else is a manifest, and a manifest goes back to our own
/// parsers rather than being flattened here.
pub fn parse(json: &str, url: &str, preferred_height: u32) -> Result<Extracted, String> {
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
    let mut manifest: Option<String> = None;

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
        match protocol {
            p if p.contains("m3u8") || p.contains("dash") => {
                // The first one listed. yt-dlp orders worst-first, and for a site that
                // publishes a real master every entry names the same document anyway.
                manifest.get_or_insert_with(|| media_url.to_owned());
                continue;
            }
            "https" | "http" => {}
            _ => continue,
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
        return match manifest {
            Some(url) => Ok(Extracted::Manifest(url)),
            None => Err("Vortex couldn't find a video on that page.".to_owned()),
        };
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

    Ok(Extracted::Ladder(Box::new(MediaCandidate {
        // The page, which is what this ladder is *of*. Nothing fetches it — a progressive
        // plan is built from the variant and audio URLs — but it is the stable id for the
        // stream and the handle a re-extraction would start from.
        id: url.to_owned(),
        manifest_url: url.to_owned(),
        kind: MediaKind::Progressive,
        title,
        duration_secs: duration,
        live,
        variants,
        audio,
        subtitles: Vec::new(),
        default_variant,
    })))
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

    /// The ladder, or a failure naming what came back instead.
    fn ladder(json: &str, url: &str, height: u32) -> MediaCandidate {
        match parse(json, url, height) {
            Ok(Extracted::Ladder(candidate)) => *candidate,
            other => panic!("expected a ladder of finished URLs, got {other:?}"),
        }
    }

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
        let candidate = ladder(DUMP, "https://example.com/watch", 1080);
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
        let candidate = ladder(YOUTUBE, "https://example.com/watch", 1080);
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
        let candidate = ladder(YOUTUBE, "https://example.com/watch", 1080);
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
        let candidate = ladder(dump, "https://example.com/watch", 1080);
        assert_eq!(candidate.variants.len(), 2);
    }

    #[test]
    fn an_hls_format_hands_the_manifest_back_to_our_own_parser() {
        // Not flattened into a one-rung ladder: our parsers read the master and build the
        // real one, with the audio renditions and subtitle tracks an extractor's list of
        // formats has already thrown away.
        let dump = r#"{"title":"Live","is_live":true,"formats":[
          {"url":"https://cdn.example/master.m3u8","protocol":"m3u8_native","vcodec":"avc1","acodec":"mp4a","height":720,"tbr":2500.0}]}"#;
        match parse(dump, "https://example.com/live", 1080).unwrap() {
            Extracted::Manifest(url) => assert_eq!(url, "https://cdn.example/master.m3u8"),
            other => panic!("a playlist is not a file to fetch: {other:?}"),
        }
    }

    #[test]
    fn a_page_that_publishes_both_families_is_not_one_ladder() {
        // What YouTube answers with today: thirty-odd finished URLs *and* a dozen HLS
        // playlists for the same video. Mixed into one ladder, a rung that is a playlist
        // sits next to a rung that is an MP4 with the same number written on it, and
        // whichever the extractor happened to list last decides how *all* of them are
        // fetched — which is a plan that fetches the page as if it were a manifest.
        let dump = r#"{"title":"Both","formats":[
          {"url":"https://cdn/720.mp4","protocol":"https","vcodec":"avc1","acodec":"none","height":720,"tbr":1200.0},
          {"url":"https://cdn/audio.m4a","protocol":"https","vcodec":"none","acodec":"mp4a.40.2","tbr":128.0},
          {"url":"https://manifest.example/hls_playlist/720","protocol":"m3u8_native","vcodec":"avc1","acodec":"none","height":720,"tbr":1200.0}]}"#;
        let candidate = ladder(dump, "https://example.com/watch", 720);
        assert_eq!(candidate.kind, MediaKind::Progressive);
        assert_eq!(
            candidate.variants.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["https://cdn/720.mp4"],
            "the playlist is a different kind of thing and does not belong on this ladder"
        );
        assert_eq!(candidate.audio.len(), 1);
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
        assert!(
            !FORWARDED.contains(&"cookie"),
            "argv is world-readable; the session travels in the jar or not at all"
        );
    }

    #[test]
    fn a_jar_carries_the_session_and_says_who_it_belongs_to() {
        let jar = render("SID=abc; __Secure-3PSID=def", "https://www.youtube.com/watch?v=x")
            .expect("two cookies is a jar worth writing");
        assert!(
            jar.starts_with("# Netscape HTTP Cookie File\n"),
            "a jar without the magic line is refused, not read as empty: {jar}"
        );

        let rows: Vec<Vec<&str>> = jar
            .lines()
            .skip(1)
            .map(|line| line.split('\t').collect())
            .collect();
        assert!(rows.iter().all(|r| r.len() == 7), "seven tab-separated fields: {jar}");
        assert_eq!(
            rows.iter().map(|r| r[0]).collect::<Vec<_>>(),
            [".youtube.com", ".youtube.com"],
            "the site the tab is part of, once per cookie, and nothing wider"
        );
        // The regression this test exists for. Two domain lines per cookie is not a
        // harmless belt-and-braces: both match `www.youtube.com`, so every cookie went out
        // twice, and a doubled `SID`/`SAPISID` is how a signed-in page stopped extracting.
        let mut names: Vec<&str> = rows.iter().map(|r| r[5]).collect();
        names.sort_unstable();
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(names, unique, "a cookie written twice is sent twice: {jar}");
        assert_eq!(rows[0][5], "SID");
        assert_eq!(rows[0][6], "abc");
        // Ahead of now, or Python's reader drops exactly the session cookies that matter.
        let expiry: u64 = rows[0][4].parse().unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(expiry > now, "{expiry} is not in the future");
    }

    #[test]
    fn a_jar_is_only_written_when_there_is_a_session_in_it() {
        assert!(render("", "https://example.com/watch").is_none());
        assert!(render("   ", "https://example.com/watch").is_none());
        assert!(render("SID=abc", "not a url").is_none());
        // A value carrying the separator would make the rest of the file unreadable, so
        // the cookie goes rather than the jar.
        let jar = render("good=1; bad=a\tb", "https://example.com/x").unwrap();
        assert_eq!(jar.lines().count(), 2, "{jar}");
        assert!(jar.contains("\tgood\t1\n"));
    }

    #[test]
    fn a_runtime_is_named_by_the_binary_and_not_by_hope() {
        // The name is the dialect yt-dlp speaks to it; a path is not enough to guess one,
        // and a wrong guess is a warning and a silent fall back to the degraded path.
        assert_eq!(runtime_named(Path::new("/usr/bin/node")), Some("node"));
        assert_eq!(runtime_named(Path::new(r"C:\tools\Deno.exe")), Some("deno"));
        assert_eq!(runtime_named(Path::new("/opt/python3")), None);
        // QuickJS ships under a different word than the one yt-dlp knows it by, and the
        // bundled sidecar lands as `qjs`. Both spellings resolve to the name yt-dlp wants.
        assert_eq!(runtime_named(Path::new(r"C:\Vortex\qjs.exe")), Some("quickjs"));
        assert_eq!(runtime_named(Path::new("/usr/local/bin/quickjs")), Some("quickjs"));
        assert!(
            !JS_RUNTIMES.iter().any(|(name, _)| *name == "bun"),
            "yt-dlp refuses every bun after 1.3.14; choosing one is choosing a failure"
        );

        let runtime = JsRuntime {
            name: "node",
            path: PathBuf::from("/usr/bin/node"),
        };
        assert_eq!(runtime.argument(), "node:/usr/bin/node");
    }

    #[test]
    fn a_bare_host_is_not_widened_towards_its_suffix() {
        let jar = render("a=1", "https://videos.example.co.uk/x").unwrap();
        assert_eq!(
            jar.lines().nth(1).unwrap().split('\t').next(),
            Some(".videos.example.co.uk"),
            "only a `www.`/`m.` prefix is dropped; anything else could reach a whole suffix"
        );
    }
}
