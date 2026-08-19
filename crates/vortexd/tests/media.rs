//! A stream, through the daemon, over the pipe — the same path the page overlay uses.
//!
//! The media pipeline has its own end-to-end tests in `vortex-media`. What these check is
//! the seam: that "what streams are here?" and "download this one" are ordinary commands,
//! and that a media job is an ordinary job once it is running.

mod harness;

use harness::{Client, Daemon, PATIENCE};
use hostile_server::media;
use vortex_proto::{
    Category, Command, ContainerPreference, Event, JobId, JobSpec, MediaKind, MediaSelection,
    Outcome, RequestEnvelope,
};

fn finished(job: JobId) -> impl Fn(&Event) -> Option<Outcome> {
    move |event| match event {
        Event::JobFinished { job: id, outcome } if *id == job => Some(outcome.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn a_stream_is_found_and_downloaded_through_the_daemon() {
    let source = tempfile::tempdir().unwrap();
    if !media::build_hls(source.path(), false) {
        eprintln!("no ffmpeg on this machine; skipping");
        return;
    }
    let origin = media::Origin::serve(source.path()).await;

    let data = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(data.path(), downloads.path()).await;
    let mut client = Client::connect(&daemon.endpoint).await;

    // ── The overlay's question ──────────────────────────────────────────────
    let mut envelope = RequestEnvelope::new(origin.url("master.m3u8"));
    envelope.page_title = Some("Colour Bars | Example".into());
    envelope.set_header("Referer", "https://example.com/watch");
    envelope.cookies = Some("session=secret".into());
    client
        .send(Command::ProbeMedia {
            envelope: envelope.clone(),
        })
        .await;

    let candidates = client
        .expect(|event| match event {
            Event::MediaFound { candidates, .. } => Some(candidates.clone()),
            Event::Error { message } => panic!("the media probe failed: {message}"),
            _ => None,
        })
        .await;
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(candidate.kind, MediaKind::Hls);
    assert_eq!(candidate.title, "Colour Bars");
    assert_eq!(candidate.variants.len(), 1);
    assert_eq!(
        candidate.audio.len(),
        1,
        "the separate audio rendition has to reach the overlay, or the user cannot pick it"
    );

    // ── The overlay's Download button ───────────────────────────────────────
    let job = client
        .submit(JobSpec {
            media: Some(MediaSelection {
                manifest_url: candidate.manifest_url.clone(),
                kind: candidate.kind,
                title: candidate.title.clone(),
                variant_id: candidate.variants[0].id.clone(),
                audio_id: Some(candidate.audio[0].id.clone()),
                subtitle_ids: Vec::new(),
                container: ContainerPreference::Auto,
            }),
            ..JobSpec::file(envelope)
        })
        .await;

    let added = client
        .expect(|event| match event {
            Event::JobAdded { job: view } if view.id == job => Some(view.clone()),
            _ => None,
        })
        .await;
    assert_eq!(
        added.category,
        Category::Video,
        "a stream is video whatever its title ends in"
    );

    let outcome = tokio::time::timeout(PATIENCE, client.expect(finished(job)))
        .await
        .expect("the media job never finished");
    let path = match outcome {
        Outcome::Completed { path, bytes, .. } => {
            assert!(bytes > 0);
            path
        }
        other => panic!("the stream did not download: {other:?}"),
    };

    let probe = media::ffprobe(std::path::Path::new(&path));
    assert!(probe.has("video"), "{:?}", probe.streams);
    assert!(probe.has("audio"), "the daemon produced a silent file: {:?}", probe.streams);

    // The work directory is a `.vxpart` like any other partial, and it goes when it is done.
    assert!(
        !std::path::Path::new(&format!("{path}.vxpart")).exists(),
        "the segments outlived the mux"
    );

    daemon.stop().await;
}

#[tokio::test]
async fn a_protected_stream_is_refused_with_the_system_named() {
    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=320x240\nv.m3u8\n",
    )
    .unwrap();
    std::fs::write(
        source.path().join("v.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example/k\",KEYFORMAT=\"com.widevine.alpha\"\n\
         #EXTINF:2.0,\nv0.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    std::fs::write(source.path().join("v0.ts"), b"never fetched").unwrap();
    let origin = media::Origin::serve(source.path()).await;

    let data = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(data.path(), downloads.path()).await;
    let mut client = Client::connect(&daemon.endpoint).await;

    client
        .send(Command::ProbeMedia {
            envelope: RequestEnvelope::new(origin.url("master.m3u8")),
        })
        .await;

    // The refusal is the answer, not an empty ladder and not a fallback to yt-dlp.
    let message = client
        .expect(|event| match event {
            Event::Error { message } => Some(message.clone()),
            Event::MediaFound { .. } => panic!("a DRM stream was offered for download"),
            _ => None,
        })
        .await;
    assert_eq!(
        message,
        "This video is protected by Widevine. Vortex doesn't download DRM."
    );
    assert_eq!(origin.hits("v0.ts"), 0, "a segment of protected media was fetched");

    daemon.stop().await;
}

#[tokio::test]
async fn retry_mux_is_offered_only_when_there_is_something_to_combine() {
    let data = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(data.path(), downloads.path()).await;
    let mut client = Client::connect(&daemon.endpoint).await;

    let job = client
        .submit(JobSpec {
            start_paused: true,
            ..JobSpec::file(RequestEnvelope::new("https://example.invalid/a.iso"))
        })
        .await;
    client.send(Command::RetryMux { job }).await;
    let message = client
        .expect(|event| match event {
            Event::Error { message } => Some(message.clone()),
            _ => None,
        })
        .await;
    assert!(message.contains("nothing to combine"), "{message}");

    daemon.stop().await;
}
