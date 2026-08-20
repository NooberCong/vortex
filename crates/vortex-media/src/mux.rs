//! Stage 7: ffmpeg (04 §7).
//!
//! A separate process, invoked with an argv array — never a shell string, because segment
//! and output paths come from a manifest and a page title (01 §Security boundaries). LGPL
//! sidecar, no linking, so the licence question stays a packaging question.
//!
//! Two behaviours matter to the person watching:
//!
//! * **`-c copy`, always.** Re-encoding is slow, lossy, and not what was asked for.
//! * **Progress is real.** A 4 GB remux takes 20–60 s, and an unexplained stall at 100% is
//!   the most common "it's broken" report in every downloader ever shipped. ffmpeg's
//!   `-progress pipe:1` is parsed and surfaced as its own phase.

use crate::plan::{Container, Plan, Role};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Where ffmpeg came from, so the failure message can say what was missing.
#[derive(Debug, Clone)]
pub struct Ffmpeg(PathBuf);

impl Ffmpeg {
    /// A bundled sidecar beside the daemon wins, then `VORTEX_FFMPEG`, then `PATH`. The
    /// bundled one wins because it is the build whose licence and codecs we know.
    pub fn find() -> Option<Self> {
        let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        if let Some(beside) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
            .filter(|path| path.is_file())
        {
            return Some(Self(beside));
        }
        if let Some(configured) = std::env::var_os("VORTEX_FFMPEG").map(PathBuf::from) {
            if configured.is_file() {
                return Some(Self(configured));
            }
        }
        which(name).map(Self)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Looks up a bare name on `PATH`. `std::process::Command` would do this for us, but then
/// "ffmpeg is not installed" only surfaces as a spawn failure halfway through a job.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[derive(Debug)]
pub enum MuxError {
    /// No ffmpeg anywhere. The one failure that is about the installation, not the video.
    Missing,
    /// ffmpeg ran and refused. Carries the tail of its diagnostics for the `Retry mux` row.
    Failed(String),
    Stopped,
}

impl std::fmt::Display for MuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MuxError::Missing => f.write_str(
                "Vortex couldn't find ffmpeg, so it can't combine the video and audio.",
            ),
            MuxError::Failed(detail) => write!(f, "Combining the tracks failed. {detail}"),
            MuxError::Stopped => f.write_str("Combining was stopped."),
        }
    }
}

/// A growing argv. A closure would be tidier but would hold the vector borrowed for the
/// whole function.
#[derive(Default)]
struct Argv(Vec<std::ffi::OsString>);

impl Argv {
    fn flag(&mut self, arg: &str) {
        self.0.push(arg.into());
    }
    fn value(&mut self, arg: impl Into<std::ffi::OsString>) {
        self.0.push(arg.into());
    }
}

/// Builds the argv for one mux. Split out from [`run`] so it can be asserted on without a
/// 4 GB file and an ffmpeg install.
pub fn arguments(plan: &Plan, work_dir: &Path, output: &Path) -> Vec<std::ffi::OsString> {
    let mut args = Argv::default();

    args.flag("-nostdin");
    args.flag("-hide_banner");
    args.flag("-loglevel");
    args.flag("error");
    args.flag("-y");

    for track in &plan.tracks {
        args.flag("-i");
        args.value(work_dir.join(&track.file));
    }

    let has_audio = plan.tracks.iter().any(|t| t.role == Role::Audio);
    let mut audio_index = 0;
    let mut subtitle_index = 0;
    for (input, track) in plan.tracks.iter().enumerate() {
        let input = input.to_string();
        match track.role {
            // With no separate audio rendition the video input is a muxed TS: take all of
            // it rather than dropping the sound the file already contains.
            Role::Video if !has_audio => {
                args.flag("-map");
                args.value(input);
            }
            Role::Video => {
                args.flag("-map");
                args.value(format!("{input}:v:0"));
            }
            Role::Audio => {
                args.flag("-map");
                args.value(format!("{input}:a:0"));
                if let Some(language) = track.language.as_deref().map(iso639_3) {
                    args.value(format!("-metadata:s:a:{audio_index}"));
                    args.value(format!("language={language}"));
                }
                audio_index += 1;
            }
            Role::Subtitle => {
                args.flag("-map");
                args.value(format!("{input}:s:0"));
                if let Some(language) = track.language.as_deref().map(iso639_3) {
                    args.value(format!("-metadata:s:s:{subtitle_index}"));
                    args.value(format!("language={language}"));
                }
                subtitle_index += 1;
            }
        }
    }

    args.flag("-c");
    args.flag("copy");
    if plan.tracks.iter().any(|t| t.role == Role::Subtitle) {
        args.flag("-c:s");
        // WebVTT is not a container-native format in either target; ffmpeg converts it.
        match plan.container {
            Container::Mp4 => args.flag("mov_text"),
            Container::Mkv => args.flag("srt"),
        }
    }
    if plan.container == Container::Mp4 {
        // Seekable the moment it finishes, rather than after the player reads the tail.
        args.flag("-movflags");
        args.flag("+faststart");
    }

    args.flag("-f");
    match plan.container {
        Container::Mp4 => args.flag("mp4"),
        Container::Mkv => args.flag("matroska"),
    }
    args.flag("-progress");
    args.flag("pipe:1");
    args.value(output);
    args.0
}

/// Runs the mux, reporting 0–100 as ffmpeg makes its way through the timeline.
pub async fn run(
    ffmpeg: &Ffmpeg,
    plan: &Plan,
    work_dir: &Path,
    output: &Path,
    control: &vortex_engine::job::ControlRx,
    report: &mut (dyn FnMut(u8) + Send),
) -> Result<(), MuxError> {
    let args = arguments(plan, work_dir, output);
    tracing::debug!(?args, "muxing");

    let mut child = Command::new(ffmpeg.path())
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            tracing::warn!("ffmpeg would not start: {e}");
            MuxError::Missing
        })?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let duration = plan.duration_secs.filter(|d| *d > 0.0);

    let mut progress = BufReader::new(stdout).lines();
    let mut diagnostics = BufReader::new(stderr).lines();
    let mut tail: Vec<String> = Vec::new();
    let mut percent = 0u8;
    report(0);

    loop {
        tokio::select! {
            line = progress.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(next) = parse_progress(&line, duration) {
                        if next > percent {
                            percent = next;
                            report(percent);
                        }
                    }
                }
                _ => break,
            },
            line = diagnostics.next_line() => {
                if let Ok(Some(line)) = line {
                    // Only the tail is kept: ffmpeg's diagnostics can run to megabytes and
                    // the useful sentence is always the last one.
                    tail.push(line);
                    if tail.len() > 8 {
                        tail.remove(0);
                    }
                }
            }
            _ = control.stopped() => {
                let _ = child.kill().await;
                return Err(MuxError::Stopped);
            }
        }
    }

    // Drain whatever ffmpeg said on its way out.
    while let Ok(Some(line)) = diagnostics.next_line().await {
        tail.push(line);
        if tail.len() > 8 {
            tail.remove(0);
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| MuxError::Failed(e.to_string()))?;
    if !status.success() {
        let detail = tail
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| format!("ffmpeg exited with {status}"));
        return Err(MuxError::Failed(detail));
    }
    report(100);
    Ok(())
}

/// `out_time_us=123456` → a percentage of the known duration. ffmpeg also emits
/// `progress=end`, which is the only reliable "done" signal it gives.
fn parse_progress(line: &str, duration: Option<f64>) -> Option<u8> {
    let (key, value) = line.split_once('=')?;
    match key.trim() {
        "progress" if value.trim() == "end" => Some(100),
        "out_time_us" | "out_time_ms" => {
            let duration = duration?;
            let micros: f64 = value.trim().parse().ok()?;
            // `out_time_ms` is a long-standing ffmpeg misnomer: the value is microseconds.
            let seconds = micros / 1_000_000.0;
            Some(((seconds / duration) * 100.0).clamp(0.0, 99.0) as u8)
        }
        _ => None,
    }
}

/// Containers want ISO 639-2/B. Manifests overwhelmingly carry 639-1, sometimes with a
/// region tag. Anything already three letters is passed through.
fn iso639_3(tag: &str) -> String {
    let base = tag.split(['-', '_']).next().unwrap_or(tag).to_ascii_lowercase();
    const MAP: &[(&str, &str)] = &[
        ("en", "eng"), ("de", "ger"), ("fr", "fre"), ("es", "spa"), ("it", "ita"),
        ("pt", "por"), ("nl", "dut"), ("sv", "swe"), ("no", "nor"), ("da", "dan"),
        ("fi", "fin"), ("pl", "pol"), ("cs", "cze"), ("ru", "rus"), ("uk", "ukr"),
        ("tr", "tur"), ("ar", "ara"), ("he", "heb"), ("hi", "hin"), ("ja", "jpn"),
        ("ko", "kor"), ("zh", "chi"), ("vi", "vie"), ("th", "tha"), ("id", "ind"),
    ];
    MAP.iter()
        .find(|(short, _)| *short == base)
        .map(|(_, long)| (*long).to_owned())
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Track;

    fn track(role: Role, file: &str, language: Option<&str>) -> Track {
        Track {
            role,
            language: language.map(str::to_owned),
            codecs: None,
            init: None,
            segments: Vec::new(),
            file: file.to_owned(),
            estimated_bytes: None,
            speculative: 0,
            payload: crate::plan::Payload::Append,
            contiguous: false,
        }
    }

    fn plan(tracks: Vec<Track>, container: Container) -> Plan {
        Plan {
            title: "A Lecture".into(),
            kind: vortex_proto::MediaKind::Hls,
            live: false,
            duration_secs: Some(60.0),
            tracks,
            container,
            container_note: None,
        }
    }

    fn rendered(plan: &Plan) -> Vec<String> {
        arguments(plan, Path::new("/work"), Path::new("/out.vxpart"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn separate_video_and_audio_are_both_mapped() {
        let plan = plan(
            vec![
                track(Role::Video, "video.mp4", None),
                track(Role::Audio, "audio.mp4", Some("en")),
            ],
            Container::Mp4,
        );
        let args = rendered(&plan);
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "0:v:0"), "{args:?}");
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "1:a:0"), "{args:?}");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata:s:a:0" && w[1] == "language=eng"),
            "{args:?}"
        );
        assert!(args.windows(2).any(|w| w[0] == "-c" && w[1] == "copy"), "never re-encode");
        assert!(
            args.windows(2).any(|w| w[0] == "-movflags" && w[1] == "+faststart"),
            "{args:?}"
        );
        assert!(args.windows(2).any(|w| w[0] == "-f" && w[1] == "mp4"), "{args:?}");
    }

    #[test]
    fn a_muxed_transport_stream_keeps_the_audio_it_already_has() {
        let plan = plan(vec![track(Role::Video, "video.ts", None)], Container::Mp4);
        let args = rendered(&plan);
        assert!(
            args.windows(2).any(|w| w[0] == "-map" && w[1] == "0"),
            "a lone TS input must not be narrowed to its video stream: {args:?}"
        );
    }

    #[test]
    fn subtitles_are_converted_for_the_container_they_land_in() {
        let mp4 = plan(
            vec![
                track(Role::Video, "video.mp4", None),
                track(Role::Audio, "audio.mp4", None),
                track(Role::Subtitle, "subtitle-0.vtt", Some("en-GB")),
            ],
            Container::Mp4,
        );
        let args = rendered(&mp4);
        assert!(args.windows(2).any(|w| w[0] == "-c:s" && w[1] == "mov_text"), "{args:?}");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-metadata:s:s:0" && w[1] == "language=eng"),
            "a region tag still resolves to a three-letter code: {args:?}"
        );

        let mkv = plan(
            vec![
                track(Role::Video, "video.mp4", None),
                track(Role::Audio, "audio.mp4", None),
                track(Role::Subtitle, "subtitle-0.vtt", None),
            ],
            Container::Mkv,
        );
        let args = rendered(&mkv);
        assert!(args.windows(2).any(|w| w[0] == "-c:s" && w[1] == "srt"), "{args:?}");
        assert!(args.windows(2).any(|w| w[0] == "-f" && w[1] == "matroska"), "{args:?}");
        assert!(!args.iter().any(|a| a == "+faststart"), "faststart is MP4-only");
    }

    #[test]
    fn the_output_muxer_is_named_rather_than_guessed_from_the_extension() {
        // The output is a `.vxpart` until the rename, and ffmpeg would refuse to guess.
        let plan = plan(vec![track(Role::Video, "video.mp4", None)], Container::Mkv);
        let args = rendered(&plan);
        let format = args.iter().position(|a| a == "-f").unwrap();
        assert_eq!(args[format + 1], "matroska");
        assert_eq!(args.last().unwrap(), "/out.vxpart");
    }

    #[test]
    fn progress_comes_from_the_timeline_and_ends_at_the_end() {
        assert_eq!(parse_progress("out_time_us=30000000", Some(60.0)), Some(50));
        assert_eq!(parse_progress("out_time_ms=60000000", Some(60.0)), Some(99));
        assert_eq!(parse_progress("progress=end", Some(60.0)), Some(100));
        assert_eq!(parse_progress("frame=120", Some(60.0)), None);
        assert_eq!(
            parse_progress("out_time_us=1000", None),
            None,
            "without a duration there is no percentage to claim"
        );
    }
}
