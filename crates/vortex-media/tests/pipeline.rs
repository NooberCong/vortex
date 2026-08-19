//! The media pipeline end to end: a real HLS origin, the real fetcher, real ffmpeg.
//!
//! The phase's acceptance criterion is "a playable file with correct audio" (06 §Phase 4),
//! because audio is where naive implementations fail. So every test here ends by asking
//! ffprobe what is actually inside the file, rather than checking that some bytes arrived.

use hostile_server::media::{self as harness, Origin};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use vortex_engine::job::{self, Engine, EngineEvent, Interrupted};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_media::job::MediaSpec;
use vortex_proto::{ContainerPreference, MediaKind, MediaSelection, RequestEnvelope};

fn engine() -> Arc<Engine> {
    Arc::new(Engine::new(Transport::new(TransportConfig::default())))
}

fn selection(origin: &Origin, container: ContainerPreference) -> MediaSelection {
    MediaSelection {
        manifest_url: origin.url("master.m3u8"),
        kind: MediaKind::Hls,
        title: "Colour Bars".into(),
        variant_id: origin.url("v.m3u8"),
        audio_id: Some(origin.url("a.m3u8")),
        subtitle_ids: Vec::new(),
        container,
    }
}

fn spec(origin: &Origin, dest: &Path, container: ContainerPreference) -> MediaSpec {
    MediaSpec {
        envelope: RequestEnvelope::new(origin.url("master.m3u8")),
        selection: selection(origin, container),
        dest_dir: dest.to_path_buf(),
        filename: None,
        max_connections: 8,
        container,
    }
}

/// Drains the event channel and hands back whatever the caller asked to watch for.
fn drain(mut events: mpsc::Receiver<EngineEvent>) -> tokio::task::JoinHandle<Vec<EngineEvent>> {
    tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        seen
    })
}

#[tokio::test]
async fn a_stream_with_separate_audio_becomes_one_playable_file() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = Origin::serve(source.path()).await;
    let dest = tempfile::tempdir().unwrap();

    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);

    let outcome = vortex_media::job::transfer(
        engine(),
        spec(&origin, dest.path(), ContainerPreference::Auto),
        events,
        control_rx,
    )
    .await;
    let seen = watcher.await.unwrap();

    let done = match outcome {
        Ok(done) => done,
        Err(e) => panic!("the pipeline did not finish: {e:?}"),
    };
    assert_eq!(
        done.path.file_name().unwrap().to_string_lossy(),
        "Colour Bars.mp4",
        "the title names the file and the container names the extension"
    );

    // The whole point of the phase: audio, from its own rendition, in the output.
    let probe = harness::ffprobe(&done.path);
    assert!(probe.has("video"), "{:?}", probe.streams);
    assert!(
        probe.has("audio"),
        "the audio rendition was dropped: {:?}",
        probe.streams
    );
    assert!(
        (probe.duration - 6.0).abs() < 1.0,
        "expected about six seconds, got {}",
        probe.duration
    );

    // The work directory only survives a failure.
    let work = vortex_media::job::work_dir(&done.path);
    assert!(!work.exists(), "the segments outlived a successful mux");

    assert!(
        seen.iter().any(|e| matches!(e, EngineEvent::Phase(vortex_proto::JobState::Muxing))),
        "muxing is its own phase, and the UI shows it"
    );
    assert!(
        seen.iter().any(|e| matches!(e, EngineEvent::Media(s) if s.segments_done > 0)),
        "segment counts are reported while they arrive"
    );
}

#[tokio::test]
async fn an_aes_128_stream_decrypts_in_flight() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), true) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    // Sanity: the fixture really is encrypted, or the test proves nothing.
    let playlist = std::fs::read_to_string(source.path().join("v.m3u8")).unwrap();
    assert!(playlist.contains("#EXT-X-KEY:METHOD=AES-128"), "{playlist}");

    let origin = Origin::serve(source.path()).await;
    let dest = tempfile::tempdir().unwrap();
    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);

    let outcome = vortex_media::job::transfer(
        engine(),
        spec(&origin, dest.path(), ContainerPreference::Auto),
        events,
        control_rx,
    )
    .await;
    watcher.await.unwrap();

    let done = outcome.expect("an encrypted stream is still a downloadable stream");
    let probe = harness::ffprobe(&done.path);
    assert!(
        probe.has("video") && probe.has("audio"),
        "decryption produced something ffmpeg could not read: {:?}",
        probe.streams
    );
    assert_eq!(origin.hits("enc.key"), 1, "the key is fetched once, not per segment");
}

#[tokio::test]
async fn a_paused_stream_resumes_without_re_fetching_a_segment() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = Origin::serve(source.path()).await;
    let dest = tempfile::tempdir().unwrap();

    // Stop as soon as the first segment is on disk.
    let (events, mut incoming) = mpsc::channel(64);
    let (control, control_rx) = job::control();
    tokio::spawn(async move {
        while let Some(event) = incoming.recv().await {
            if let EngineEvent::Media(summary) = event {
                if summary.segments_done >= 1 {
                    control.stop();
                }
            }
        }
    });
    let stopped = vortex_media::job::transfer(
        engine(),
        spec(&origin, dest.path(), ContainerPreference::Auto),
        events,
        control_rx,
    )
    .await;
    assert!(
        matches!(stopped, Err(Interrupted::Stopped)),
        "expected a resumable stop, got {stopped:?}"
    );

    let first_pass: Vec<u64> = (0..3).map(|i| origin.hits(&format!("v{i}.ts"))).collect();
    let fetched_once: Vec<usize> = first_pass
        .iter()
        .enumerate()
        .filter(|(_, hits)| **hits > 0)
        .map(|(index, _)| index)
        .collect();
    assert!(!fetched_once.is_empty(), "nothing was fetched before the stop");

    // Second pass: same destination, same work directory.
    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);
    let done = vortex_media::job::transfer(
        engine(),
        spec(&origin, dest.path(), ContainerPreference::Auto),
        events,
        control_rx,
    )
    .await
    .expect("the second pass finishes");
    watcher.await.unwrap();

    for index in fetched_once {
        let path = format!("v{index}.ts");
        // Segments the first pass got *and wrote* are not asked for again. A segment that
        // was in flight when the stop landed is not durable and may legitimately repeat.
        assert!(
            origin.hits(&path) <= 2,
            "{path} was fetched {} times across a pause",
            origin.hits(&path)
        );
    }

    let probe = harness::ffprobe(&done.path);
    assert!(probe.has("audio"), "{:?}", probe.streams);
    assert!((probe.duration - 6.0).abs() < 1.0, "{}", probe.duration);
}

#[tokio::test]
async fn a_missing_segment_is_a_gap_not_a_lost_download() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = Origin::serve(source.path()).await;
    // 410 is fatal for one segment and says nothing about the other 1,799.
    std::fs::remove_file(source.path().join("v1.ts")).unwrap();

    let dest = tempfile::tempdir().unwrap();
    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);

    let outcome = vortex_media::job::transfer(
        engine(),
        spec(&origin, dest.path(), ContainerPreference::Auto),
        events,
        control_rx,
    )
    .await;
    let seen = watcher.await.unwrap();

    let done = outcome.expect("one bad segment must not discard the rest");
    assert!(done.bytes > 0);
    let missing = seen
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Media(summary) => Some(summary.segments_missing),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    assert!(missing >= 1, "the gap was never reported to the user");
    assert!(
        harness::ffprobe(&done.path).has("video"),
        "what did arrive is still a playable file"
    );
}

#[tokio::test]
async fn a_protected_manifest_is_refused_before_anything_is_fetched() {
    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=320x240\nv.m3u8\n",
    )
    .unwrap();
    std::fs::write(
        source.path().join("v.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
         #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n\
         #EXTINF:2.0,\nv0.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    std::fs::write(source.path().join("v0.ts"), b"never fetched").unwrap();

    let origin = Origin::serve(source.path()).await;
    let dest = tempfile::tempdir().unwrap();
    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);

    let outcome = vortex_media::job::transfer(
        engine(),
        MediaSpec {
            selection: MediaSelection {
                audio_id: None,
                ..selection(&origin, ContainerPreference::Auto)
            },
            ..spec(&origin, dest.path(), ContainerPreference::Auto)
        },
        events,
        control_rx,
    )
    .await;
    watcher.await.unwrap();

    match outcome {
        Err(Interrupted::Failed(e)) => {
            // The sentence names the system, and is not rewritten into a server error.
            assert_eq!(
                e.user_message(),
                "This video is protected by FairPlay. Vortex doesn't download DRM."
            );
        }
        other => panic!("a DRM stream must be refused, got {other:?}"),
    }
    assert_eq!(origin.hits("v0.ts"), 0, "a segment of protected media was fetched");
}

#[tokio::test]
async fn a_transient_failure_is_retried_rather_than_recorded_as_a_gap() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = Origin::serve(source.path()).await;
    origin.fail_once("v1.ts", 503);

    let dest = tempfile::tempdir().unwrap();
    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);

    let done = tokio::time::timeout(
        Duration::from_secs(60),
        vortex_media::job::transfer(
            engine(),
            spec(&origin, dest.path(), ContainerPreference::Auto),
            events,
            control_rx,
        ),
    )
    .await
    .expect("the pipeline hung")
    .expect("a 503 is transient, not terminal");
    let seen = watcher.await.unwrap();

    assert!(origin.hits("v1.ts") >= 2, "the segment was never retried");
    let missing = seen
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Media(summary) => Some(summary.segments_missing),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    assert_eq!(missing, 0, "a retried segment is not a gap");
    assert!(harness::ffprobe(&done.path).has("audio"));
}

#[tokio::test]
async fn a_dash_manifest_ffmpeg_itself_wrote_round_trips() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_dash(source.path()) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = Origin::serve(source.path()).await;
    let dest = tempfile::tempdir().unwrap();
    let engine = engine();

    // The ladder as the overlay would show it, from the real MPD.
    let mut envelope = RequestEnvelope::new(origin.url("manifest.mpd"));
    envelope.page_title = Some("Colour Bars | Example".into());
    let candidate = vortex_media::plan::inspect(&engine, &envelope, 1080)
        .await
        .expect("the MPD parses");
    assert_eq!(candidate.kind, MediaKind::Dash);
    assert!(!candidate.live);
    assert_eq!(candidate.variants.len(), 1, "one video representation");
    assert_eq!(candidate.variants[0].height, Some(240));
    assert_eq!(
        candidate.audio.len(),
        1,
        "the audio adaptation set has to survive the parse or the file comes out silent"
    );
    assert!(
        (candidate.duration_secs.unwrap_or_default() - 6.0).abs() < 1.0,
        "{:?}",
        candidate.duration_secs
    );

    let selection = MediaSelection {
        manifest_url: candidate.manifest_url.clone(),
        kind: candidate.kind,
        title: candidate.title.clone(),
        variant_id: candidate.variants[0].id.clone(),
        audio_id: Some(candidate.audio[0].id.clone()),
        subtitle_ids: Vec::new(),
        container: ContainerPreference::Auto,
    };

    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);
    let done = vortex_media::job::transfer(
        engine,
        MediaSpec {
            envelope,
            selection,
            dest_dir: dest.path().to_path_buf(),
            filename: None,
            max_connections: 8,
            container: ContainerPreference::Auto,
        },
        events,
        control_rx,
    )
    .await
    .expect("the DASH pipeline finishes");
    watcher.await.unwrap();

    // ffmpeg's own audio runs to four segments where `duration / @duration` says three,
    // because AAC frames do not divide evenly into two seconds. Trusting the arithmetic
    // would silently drop the last two seconds of sound.
    assert!(
        origin.hits("chunk-stream1-00004.m4s") >= 1,
        "the speculative tail was never probed, so the audio is short"
    );
    assert_eq!(
        origin.hits("chunk-stream1-00005.m4s"),
        1,
        "probing stops at the first segment the server does not have"
    );

    // The init segments are the part naive implementations forget; without them the
    // concatenated fragments are unreadable and this assertion is what catches it.
    let probe = harness::ffprobe(&done.path);
    assert!(probe.has("video"), "{:?}", probe.streams);
    assert!(probe.has("audio"), "{:?}", probe.streams);
    assert!((probe.duration - 6.0).abs() < 1.0, "{}", probe.duration);
}

#[tokio::test]
async fn the_hls_ladder_is_what_the_overlay_would_show() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = Origin::serve(source.path()).await;
    let mut envelope = RequestEnvelope::new(origin.url("master.m3u8"));
    envelope.page_title = Some("Colour Bars | Example".into());

    let candidate = vortex_media::plan::inspect(&engine(), &envelope, 1080)
        .await
        .expect("the master playlist parses");

    assert_eq!(candidate.kind, MediaKind::Hls);
    assert_eq!(candidate.title, "Colour Bars", "the site name is stripped");
    assert_eq!(candidate.variants.len(), 1);
    assert_eq!(candidate.variants[0].height, Some(240));
    assert_eq!(candidate.default_variant, Some(0));
    assert_eq!(candidate.audio.len(), 1);
    assert_eq!(candidate.audio[0].language.as_deref(), Some("en"));
    assert!(candidate.audio[0].default);
    assert_eq!(candidate.subtitles.len(), 1, "the overlay offers what the manifest carries");
    assert_eq!(candidate.subtitles[0].language.as_deref(), Some("en"));
    // bandwidth x duration / 8, marked as an estimate by the UI, not by the number.
    let estimate = candidate.variants[0].estimated_bytes.unwrap();
    assert!((400_000..600_000).contains(&estimate), "{estimate}");
}

#[tokio::test]
async fn subtitles_are_stitched_and_embedded_as_a_soft_track() {
    let source = tempfile::tempdir().unwrap();
    if !harness::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = harness::Origin::serve(source.path()).await;
    let dest = tempfile::tempdir().unwrap();

    let mut chosen = selection(&origin, ContainerPreference::Auto);
    chosen.subtitle_ids = vec![origin.url("s.m3u8")];

    let (events, incoming) = mpsc::channel(64);
    let (_control, control_rx) = job::control();
    let watcher = drain(incoming);
    let done = vortex_media::job::transfer(
        engine(),
        MediaSpec {
            selection: chosen,
            ..spec(&origin, dest.path(), ContainerPreference::Auto)
        },
        events,
        control_rx,
    )
    .await
    .expect("a subtitle rendition must not take the video down with it");
    watcher.await.unwrap();

    let probe = harness::ffprobe(&done.path);
    assert!(probe.has("video"), "{:?}", probe.streams);
    assert!(probe.has("audio"), "{:?}", probe.streams);
    assert!(
        probe.has("subtitle"),
        "the subtitle track never reached the file: {:?}",
        probe.streams
    );
}

#[test]
fn stitched_webvtt_puts_every_cue_where_the_timestamp_map_says() {
    // Three one-and-a-half-second cues, each in its own segment, each declaring a local
    // clock two seconds later than the last. Concatenated naively they would all sit at
    // zero and the file would carry three `WEBVTT` headers.
    let segments: Vec<Vec<u8>> = (0..3)
        .map(|i| {
            format!(
                "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:{},LOCAL:00:00:00.000\n\n\
                 00:00:00.000 --> 00:00:01.500\nLine {i}\n",
                i * 180_000
            )
            .into_bytes()
        })
        .collect();

    let mut document = String::new();
    for (index, segment) in segments.iter().enumerate() {
        document.push_str(&String::from_utf8(vortex_media::vtt::stitch(segment, index == 0)).unwrap());
    }

    assert_eq!(document.matches("WEBVTT").count(), 1, "{document}");
    assert!(document.contains("00:00:00.000 --> 00:00:01.500"), "{document}");
    assert!(document.contains("00:00:02.000 --> 00:00:03.500"), "{document}");
    assert!(document.contains("00:00:04.000 --> 00:00:05.500"), "{document}");
}
