//! A well-behaved media origin, and the fixtures to fill it.
//!
//! The rest of this crate lies on purpose. This module does not: it serves a directory
//! honestly and counts what was asked for, which is what turns "the resume worked" from an
//! impression into an assertion. It lives here rather than in one test suite because both
//! `vortex-media` and `vortexd` need a real HLS origin, and two copies of a test server is
//! two things to keep in step.
//!
//! The media itself is made by ffmpeg. Hand-written fixtures prove a parser against what
//! its author imagined; ffmpeg's output proves it against what encoders actually emit.

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Shared {
    root: PathBuf,
    hits: Arc<Mutex<HashMap<String, u64>>>,
    failures: Arc<Mutex<HashMap<String, u16>>>,
}

pub struct Origin {
    pub base: String,
    shared: Shared,
}

impl Origin {
    /// Serves `root` over loopback for as long as the process lives.
    pub async fn serve(root: &Path) -> Self {
        let shared = Shared {
            root: root.to_path_buf(),
            hits: Arc::default(),
            failures: Arc::default(),
        };
        let app = Router::new()
            .route("/{*path}", axum::routing::get(handler))
            .with_state(shared.clone());

        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("binding a loopback port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Self {
            base: format!("http://{addr}"),
            shared,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    /// How many times `path` has been requested. The number that makes "it resumed"
    /// checkable.
    pub fn hits(&self, path: &str) -> u64 {
        self.shared
            .hits
            .lock()
            .expect("hit counter")
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    /// The next request for `path` gets `status` instead of the file.
    pub fn fail_once(&self, path: &str, status: u16) {
        self.shared
            .failures
            .lock()
            .expect("failure map")
            .insert(path.to_owned(), status);
    }
}

async fn handler(State(shared): State<Shared>, UrlPath(path): UrlPath<String>) -> Response {
    *shared
        .hits
        .lock()
        .expect("hit counter")
        .entry(path.clone())
        .or_default() += 1;

    if let Some(status) = shared.failures.lock().expect("failure map").remove(&path) {
        return Response::builder()
            .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
            .body(Body::empty())
            .expect("a status-only response");
    }

    // A test server that can be asked for `../../etc/passwd` is a bad habit written down.
    let mut file = shared.root.clone();
    for part in path.split('/').filter(|p| *p != ".." && !p.is_empty()) {
        file.push(part);
    }
    match std::fs::read(&file) {
        Ok(body) => Response::builder()
            .status(StatusCode::OK)
            .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
            .body(Body::from(body))
            .expect("a file response"),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("a 404"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixtures
// ─────────────────────────────────────────────────────────────────────────────

/// Finds `ffmpeg`/`ffprobe`. Returns `None` rather than panicking so a machine without
/// them skips loudly instead of failing a correct build.
pub fn tool(name: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if let Some(configured) = std::env::var_os(format!(
        "VORTEX_{}",
        name.trim_end_matches(".exe").to_ascii_uppercase()
    )) {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&name))
        .find(|candidate| candidate.is_file())
}

/// ffmpeg's path arithmetic looks for `/`. Handed a Windows path it finds none, decides the
/// output directory is the working directory, and scatters segments across the source tree.
fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Six seconds of colour bars and a 440 Hz tone, cut into 2-second HLS segments with the
/// audio in a **separate rendition** — the shape that makes naive downloaders produce
/// silence, and therefore the shape worth testing against.
pub fn build_hls(dir: &Path, encrypted: bool) -> bool {
    let Some(ffmpeg) = tool("ffmpeg") else {
        return false;
    };
    std::fs::create_dir_all(dir).expect("the fixture directory");

    let mut video: Vec<String> = common_args()
        .into_iter()
        .chain(
            [
                "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=6",
                "-c:v", "libx264", "-preset", "ultrafast", "-g", "10", "-t", "6", "-an",
                "-f", "hls", "-hls_time", "2", "-hls_playlist_type", "vod",
                "-hls_segment_filename",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .collect();
    video.push(slashes(&dir.join("v%d.ts")));
    if encrypted {
        video.push("-hls_key_info_file".to_owned());
        video.push(slashes(&dir.join("key_info")));
    }
    video.push(slashes(&dir.join("v.m3u8")));

    let mut audio: Vec<String> = common_args()
        .into_iter()
        .chain(
            [
                "-f", "lavfi", "-i", "sine=frequency=440:duration=6",
                "-c:a", "aac", "-t", "6", "-vn",
                "-f", "hls", "-hls_time", "2", "-hls_playlist_type", "vod",
                "-hls_segment_filename",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .collect();
    audio.push(slashes(&dir.join("a%d.ts")));
    audio.push(slashes(&dir.join("a.m3u8")));

    if encrypted {
        std::fs::write(dir.join("enc.key"), [0x11u8; 16]).expect("the key");
        // ffmpeg's key-info file: the URI to publish, the local key, and the IV.
        std::fs::write(
            dir.join("key_info"),
            format!(
                "enc.key\n{}\n0123456789abcdef0123456789abcdef\n",
                slashes(&dir.join("enc.key"))
            ),
        )
        .expect("the key info");
    }

    for args in [video, audio] {
        run(&ffmpeg, &args);
    }

    write_subtitles(dir);
    std::fs::write(
        dir.join("master.m3u8"),
        "#EXTM3U\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",DEFAULT=YES,LANGUAGE=\"en\",URI=\"a.m3u8\"\n\
         #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",DEFAULT=YES,AUTOSELECT=YES,LANGUAGE=\"en\",URI=\"s.m3u8\"\n\
         #EXT-X-STREAM-INF:BANDWIDTH=600000,RESOLUTION=320x240,CODECS=\"avc1.42c015,mp4a.40.2\",AUDIO=\"aac\",SUBTITLES=\"subs\"\n\
         v.m3u8\n",
    )
    .expect("the master playlist");
    true
}

/// A segmented WebVTT rendition, written by hand because ffmpeg does not emit one.
///
/// Each segment repeats the `WEBVTT` header and states its own `X-TIMESTAMP-MAP`, with
/// cue times local to the segment. That is the real shape, and it is the shape that makes
/// byte-concatenation wrong: the second segment's cues say 00:00:00 and belong at 00:00:02.
fn write_subtitles(dir: &Path) {
    for (index, (mpegts, line)) in [(0u64, "First"), (180_000, "Second"), (360_000, "Third")]
        .into_iter()
        .enumerate()
    {
        std::fs::write(
            dir.join(format!("s{index}.vtt")),
            format!(
                "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:{mpegts},LOCAL:00:00:00.000\n\n\
                 00:00:00.000 --> 00:00:01.500\n{line}\n"
            ),
        )
        .expect("a subtitle segment");
    }
    std::fs::write(
        dir.join("s.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXTINF:2.0,\ns0.vtt\n#EXTINF:2.0,\ns1.vtt\n#EXTINF:2.0,\ns2.vtt\n#EXT-X-ENDLIST\n",
    )
    .expect("the subtitle playlist");
}

/// The same six seconds packaged as DASH by ffmpeg: a real `SegmentTemplate` with
/// `$Number%05d$`, real init segments, and video and audio in separate adaptation sets.
pub fn build_dash(dir: &Path) -> bool {
    let Some(ffmpeg) = tool("ffmpeg") else {
        return false;
    };
    std::fs::create_dir_all(dir).expect("the fixture directory");
    let args: Vec<String> = common_args()
        .into_iter()
        .chain(
            [
                "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=6",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=6",
                "-map", "0:v", "-map", "1:a",
                "-c:v", "libx264", "-preset", "ultrafast", "-g", "20",
                "-c:a", "aac", "-t", "6",
                "-f", "dash", "-seg_duration", "2", "-use_template", "1", "-use_timeline", "0",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .chain(std::iter::once(slashes(&dir.join("manifest.mpd"))))
        .collect();
    run(&ffmpeg, &args);
    true
}

fn common_args() -> Vec<String> {
    ["-nostdin", "-hide_banner", "-loglevel", "error", "-y"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn run(ffmpeg: &Path, args: &[String]) {
    let status = std::process::Command::new(ffmpeg)
        .args(args)
        .status()
        .expect("running ffmpeg");
    assert!(status.success(), "ffmpeg failed: {args:?}");
}

/// What ffprobe says is inside a file. "Correct audio" is the acceptance criterion for the
/// media phase, so the test asks the same tool the user's player would agree with.
pub struct Probe {
    pub streams: Vec<String>,
    pub duration: f64,
}

impl Probe {
    pub fn has(&self, kind: &str) -> bool {
        self.streams.iter().any(|s| s == kind)
    }
}

pub fn ffprobe(path: &Path) -> Probe {
    let exe = tool("ffprobe").expect("ffprobe is beside ffmpeg");
    let output = std::process::Command::new(exe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:format=duration",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("running ffprobe");
    let text = String::from_utf8_lossy(&output.stdout);
    Probe {
        streams: text
            .lines()
            .filter_map(|l| l.strip_prefix("codec_type="))
            .map(str::to_owned)
            .collect(),
        duration: text
            .lines()
            .find_map(|l| l.strip_prefix("duration="))
            .and_then(|d| d.parse().ok())
            .unwrap_or(0.0),
    }
}
