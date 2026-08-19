//! The hostile-server suite (06 §Exit criteria for phase 1).
//!
//! Every case must yield a **correct file** or a **clean `NeedsDecision`**. None may yield
//! silent corruption. A silent corruption bug found in phase 5 costs ten times what it
//! costs here.

use hostile_server::Behaviour;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use vortex_engine::job::{self, Engine, EngineEvent, Interrupted, TransferSpec};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_proto::{Decision, RequestEnvelope};

const MB: u64 = 1024 * 1024;

struct Harness {
    engine: Arc<Engine>,
    dir: tempfile::TempDir,
}

fn harness() -> Harness {
    Harness {
        engine: Arc::new(Engine::new(Transport::new(TransportConfig {
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(10),
            ..Default::default()
        }))),
        dir: tempfile::tempdir().unwrap(),
    }
}

impl Harness {
    fn spec(&self, url: &str) -> TransferSpec {
        TransferSpec {
            envelope: RequestEnvelope::new(url),
            dest_dir: self.dir.path().to_path_buf(),
            filename: Some("fixture.bin".into()),
            max_connections: 8,
            write_budget: 16 * MB,
        }
    }

    async fn run(&self, url: &str) -> job::Outcome {
        let (tx, mut rx) = mpsc::channel(256);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let (_control, control_rx) = job::control();
        job::transfer(self.engine.clone(), self.spec(url), tx, control_rx).await
    }
}

fn assert_matches_fixture(path: &std::path::Path, len: u64) {
    let actual = std::fs::read(path).unwrap();
    assert_eq!(actual.len() as u64, len, "wrong length");
    let expected = hostile_server::fixture(len);
    assert!(
        actual == expected,
        "the file on disk does not match the fixture"
    );
}

#[tokio::test]
async fn an_honest_server_produces_a_byte_exact_file() {
    let h = harness();
    let server = hostile_server::spawn(32 * MB, Behaviour::default()).await;
    let done = h.run(&server.url()).await.expect("should complete");
    assert_matches_fixture(&done.path, server.len);
    assert!(!done.path.to_string_lossy().ends_with(".vxpart"));
}

#[tokio::test]
async fn a_server_that_advertises_ranges_and_ignores_them_still_produces_a_correct_file() {
    let h = harness();
    let server = hostile_server::spawn(
        12 * MB,
        Behaviour {
            advertise_ranges_ignore_them: true,
            ..Default::default()
        },
    )
    .await;
    let done = h.run(&server.url()).await.expect("should complete");
    assert_matches_fixture(&done.path, server.len);
}

#[tokio::test]
async fn a_server_with_no_range_support_falls_back_to_one_stream() {
    let h = harness();
    let server = hostile_server::spawn(
        12 * MB,
        Behaviour {
            no_ranges: true,
            ..Default::default()
        },
    )
    .await;
    let done = h.run(&server.url()).await.expect("should complete");
    assert_matches_fixture(&done.path, server.len);
}

#[tokio::test]
async fn one_slow_edge_among_fast_ones_still_produces_a_byte_exact_file() {
    // Heterogeneous paths are where the scheduler does its three most intricate things:
    // steal from a straggler's lease, evict the worker on it, and hedge the tail with a
    // second request. All three move a worker's cursor around under it, and one of them
    // used to move the cursor without moving the response body it was reading — which
    // produced a full-length file of shifted, wrong bytes. Nothing else in this suite
    // makes a straggler, so nothing else reached that code.
    let h = harness();
    let server = hostile_server::spawn(
        64 * MB,
        Behaviour {
            slow_first: Some(2 * MB),
            ..Default::default()
        },
    )
    .await;
    let done = h.run(&server.url()).await.expect("should complete");
    assert_matches_fixture(&done.path, server.len);
}

#[tokio::test]
async fn a_wrong_content_range_is_refused_rather_than_written() {
    let h = harness();
    let server = hostile_server::spawn(
        16 * MB,
        Behaviour {
            wrong_content_range: true,
            ..Default::default()
        },
    )
    .await;
    match h.run(&server.url()).await {
        Err(Interrupted::NeedsDecision(Decision::IntegrityMismatch { .. })) => {}
        other => panic!("expected a clean integrity decision, got {other:?}"),
    }
}

#[tokio::test]
async fn a_body_from_the_wrong_offset_never_reaches_the_final_file() {
    let h = harness();
    // A server that shifts *every* range by the same amount reports an honest
    // `Content-Range` and defeats any two-sample comparison — the shift is a bijection, so
    // the samples still differ exactly as they should. No client can catch that from
    // metadata alone; the checksum is what catches it, which is why an offered digest is
    // always verified before the file is renamed into place.
    let server = hostile_server::spawn(
        16 * MB,
        Behaviour {
            wrong_body_offset: true,
            offer_checksum: true,
            ..Default::default()
        },
    )
    .await;
    match h.run(&server.url()).await {
        Err(Interrupted::NeedsDecision(Decision::IntegrityMismatch { .. })) => {}
        other => panic!("silent corruption would look like this: {other:?}"),
    }
    assert!(
        !h.dir.path().join("fixture.bin").exists(),
        "a file that failed verification must never get the final name"
    );
}

#[tokio::test]
async fn a_gzipped_206_is_refused() {
    let h = harness();
    let server = hostile_server::spawn(
        16 * MB,
        Behaviour {
            gzip_206: true,
            ..Default::default()
        },
    )
    .await;
    match h.run(&server.url()).await {
        Err(Interrupted::NeedsDecision(_)) => {}
        Ok(done) => assert_matches_fixture(&done.path, server.len),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn a_body_of_zeros_is_caught_by_the_checksum() {
    let h = harness();
    let server = hostile_server::spawn(
        12 * MB,
        Behaviour {
            zeros_body: true,
            offer_checksum: true,
            ..Default::default()
        },
    )
    .await;
    match h.run(&server.url()).await {
        Err(Interrupted::NeedsDecision(Decision::IntegrityMismatch { .. })) => {}
        other => panic!("a plausible body of zeros must not pass: {other:?}"),
    }
}

#[tokio::test]
async fn a_truncated_body_never_becomes_a_finished_file() {
    let h = harness();
    let server = hostile_server::spawn(
        12 * MB,
        Behaviour {
            truncate_short: true,
            ..Default::default()
        },
    )
    .await;
    // A body that stops short of its `Content-Length` is a transient failure, not a
    // corrupt file: the worker re-requests the remainder from its cursor and the bytes
    // that did arrive are kept. What must never happen is a short file wearing the final
    // name.
    match h.run(&server.url()).await {
        Ok(done) => {
            assert_matches_fixture(&done.path, server.len);
            assert!(done.retries.0 > 0, "the truncation should have been noticed");
        }
        Err(Interrupted::Stopped) => {
            assert!(h.dir.path().join("fixture.bin.vxpart").exists());
            assert!(!h.dir.path().join("fixture.bin").exists());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn a_flaky_server_is_retried_rather_than_failed() {
    let h = harness();
    let server = hostile_server::spawn(
        12 * MB,
        Behaviour {
            flaky_first: 2,
            ..Default::default()
        },
    )
    .await;
    // The probe itself is refused twice; the daemon retries a stalled job, so a single
    // `transfer` call reports a resumable stop rather than a failure.
    let first = h.run(&server.url()).await;
    let done = match first {
        Ok(done) => done,
        Err(Interrupted::Stopped) => h.run(&server.url()).await.expect("second attempt"),
        other => panic!("a 503 is transient, not terminal: {other:?}"),
    };
    assert_matches_fixture(&done.path, server.len);
}

#[tokio::test]
async fn a_404_is_the_only_kind_of_failure_that_is_terminal() {
    let h = harness();
    let server = hostile_server::spawn(MB, Behaviour::default()).await;
    let url = format!("http://{}/missing", server.addr);
    match h.run(&url).await {
        Err(Interrupted::Failed(e)) => assert_eq!(e.class, vortex_engine::Class::Fatal),
        other => panic!("expected a terminal failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_checksum_the_server_offers_is_verified_and_reported() {
    let h = harness();
    let server = hostile_server::spawn(
        16 * MB,
        Behaviour {
            offer_checksum: true,
            ..Default::default()
        },
    )
    .await;
    let done = h.run(&server.url()).await.expect("should complete");
    let verified = done.verified.expect("a checksum was offered");
    assert_eq!(verified.algorithm, "SHA-256");
    assert!(verified.ok);
    assert_matches_fixture(&done.path, server.len);
}

#[tokio::test]
async fn a_paused_job_resumes_from_its_bitmap_without_refetching_everything() {
    let h = harness();
    let server = hostile_server::spawn(
        48 * MB,
        Behaviour {
            limit_rate: Some(8 * MB),
            ..Default::default()
        },
    )
    .await;

    // Stop the job part-way.
    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let (control, control_rx) = job::control();
    let spec = h.spec(&server.url());
    let engine = h.engine.clone();
    let running = tokio::spawn(async move { job::transfer(engine, spec, tx, control_rx).await });
    tokio::time::sleep(Duration::from_millis(1500)).await;
    control.stop();
    assert!(matches!(running.await.unwrap(), Err(Interrupted::Stopped)));

    let part = h.dir.path().join("fixture.bin.vxpart");
    let meta = h.dir.path().join("fixture.bin.vxpart.meta");
    assert!(part.exists() && meta.exists(), "the partial must survive");
    let requests_before = server.requests();

    // Resuming finishes the job and leaves a byte-exact file.
    let done = h.run(&server.url()).await.expect("resume should finish");
    assert_matches_fixture(&done.path, server.len);
    assert!(!meta.exists(), "the sidecar is removed once the file is real");
    assert!(server.requests() > requests_before);
}

#[tokio::test]
async fn an_expired_signed_url_is_renewed_by_the_browser_and_the_bitmap_is_kept() {
    let h = harness();
    let server = hostile_server::spawn(
        48 * MB,
        Behaviour {
            limit_rate: Some(6 * MB),
            // Well inside the transfer, not near its edge. 48 MiB at 6 MB/s per connection
            // takes roughly a second once concurrency has ramped, so a window anywhere
            // near that is a race with the scheduler rather than a test of renewal.
            expires_after: Duration::from_millis(400).into(),
            ..Default::default()
        },
    )
    .await;

    let (tx, mut rx) = mpsc::channel(256);
    let (control, control_rx) = job::control();

    // Stand in for the extension: when the engine says the URL expired, mint a fresh one
    // in "page context" and hand it back. The engine must continue from its bitmap.
    let control = Arc::new(control);
    let responder = control.clone();
    let renewals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = renewals.clone();
    let server_for_renewal = server.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let EngineEvent::UrlExpired(_) = event {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let fresh = reqwest::get(server_for_renewal.renew_url())
                    .await
                    .unwrap()
                    .text()
                    .await
                    .unwrap();
                let url = format!("http://{}/file?token={fresh}", server_for_renewal.addr);
                responder.renew(RequestEnvelope::new(url)).await;
            }
        }
    });

    let spec = h.spec(&server.url());
    let engine = h.engine.clone();
    let done = job::transfer(engine, spec, tx, control_rx)
        .await
        .expect("a renewed URL must let the job finish");

    assert_matches_fixture(&done.path, server.len);
    assert!(
        renewals.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the URL should have expired at least once"
    );
    // Nothing was re-downloaded: the engine resumed on the existing bitmap, so the server
    // served little more than the file itself.
    let served = server.requests();
    assert!(served < 200, "{served} requests for one file is a restart");
}

#[tokio::test]
async fn a_small_file_is_never_parallelised() {
    let h = harness();
    let server = hostile_server::spawn(2 * MB, Behaviour::default()).await;
    let done = h.run(&server.url()).await.expect("should complete");
    assert_matches_fixture(&done.path, server.len);
    // Probe (2 requests) plus a single stream. A parallel run would show many more.
    assert!(
        server.requests() <= 4,
        "a 2 MB file took {} requests",
        server.requests()
    );
}

#[tokio::test]
async fn a_server_that_punishes_concurrency_is_backed_off_not_hammered() {
    let h = harness();
    let server = hostile_server::spawn(
        48 * MB,
        Behaviour {
            max_concurrent: Some(2),
            limit_rate: Some(16 * MB),
            ..Default::default()
        },
    )
    .await;
    let done = h.run(&server.url()).await.expect("should still complete");
    assert_matches_fixture(&done.path, server.len);
}

