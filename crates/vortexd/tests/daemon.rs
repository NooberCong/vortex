//! The daemon, end to end: a real endpoint, real frames, and a server that misbehaves.
//!
//! These are the exit criteria for phase 3 (06 §Phase 3) — a download survives a daemon
//! restart and a signed URL expiring mid-transfer, with no user action.

mod harness;

use harness::{Client, Daemon, PATIENCE};
use hostile_server::Behaviour;
use std::time::Duration;
use vortex_proto::{
    Command, Event, JobId, JobSpec, JobState, Outcome, RequestEnvelope, SubscriptionScope,
};

/// Big enough to be a parallel transfer (02 §3 puts the floor at 8 MiB).
const SIZE: u64 = 12 << 20;

/// A server that serves at a believable speed rather than at loopback speed, so a test can
/// still see the transfer while it is happening.
fn slow() -> Behaviour {
    Behaviour {
        limit_rate: Some(1 << 20),
        ..Default::default()
    }
}

fn envelope(url: String, name: &str) -> RequestEnvelope {
    let mut envelope = RequestEnvelope::new(url);
    envelope.filename_hint = Some(name.to_owned());
    envelope.set_header("Referer", "https://example.com/downloads");
    // A credential, so the tests can prove it never reaches the database.
    envelope.cookies = Some("session=secret".into());
    envelope
}

fn finished(job: JobId) -> impl Fn(&Event) -> Option<Outcome> {
    move |event| match event {
        Event::JobFinished { job: id, outcome } if *id == job => Some(outcome.clone()),
        _ => None,
    }
}

/// What a pause is allowed to cost. Only whole blocks are journalled, so a transfer
/// stopped mid-block loses whatever each connection had in flight and re-fetches it —
/// deliberately, because a block that was in flight when the process stopped is a block
/// nothing can vouch for (02 §5).
fn in_flight_allowance(frame: &vortex_proto::ProgressFrame) -> u64 {
    frame.block_size as u64 * frame.connections.max(1) as u64
}

/// Reads until the job has more than `want` bytes down, and returns the last frame seen.
async fn progress_past(
    client: &mut Client,
    want: u64,
) -> vortex_proto::ProgressFrame {
    client
        .expect(|event| match event {
            Event::JobProgress { frame, .. } if frame.completed > want => Some(frame.clone()),
            _ => None,
        })
        .await
}

fn completed(outcome: Outcome) -> (String, u64) {
    match outcome {
        Outcome::Completed { path, bytes, .. } => (path, bytes),
        other => panic!("the download did not finish: {other:?}"),
    }
}

#[tokio::test]
async fn a_download_runs_end_to_end_over_the_pipe() {
    // Rate-limited so the transfer spans several 2 Hz ticks; over loopback an unlimited
    // 12 MiB lands before the first one, and then a summary subscriber would rightly see
    // nothing.
    let server = hostile_server::spawn(SIZE, slow()).await;
    let root = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&root.path().join("data"), &root.path().join("downloads")).await;

    let mut client = daemon.client().await;
    client
        .send(Command::Subscribe {
            scope: SubscriptionScope::Summary,
        })
        .await;
    let job = client
        .submit(JobSpec::file(envelope(server.url(), "fixture.bin")))
        .await;

    let mut saw_summary = false;
    let outcome = client
        .expect(|event| match event {
            Event::JobSummary { .. } => {
                saw_summary = true;
                None
            }
            Event::JobFinished { job: id, outcome } if *id == job => Some(outcome.clone()),
            _ => None,
        })
        .await;

    let (path, bytes) = completed(outcome);
    assert_eq!(bytes, SIZE);
    assert_eq!(
        hostile_server::sha256_hex(&std::fs::read(&path).unwrap()),
        server.sha256,
        "the file on disk is not the file the server served"
    );
    assert!(saw_summary, "a Summary subscriber saw no summary frames");
    assert!(
        !std::path::Path::new(&format!("{path}.vxpart")).exists(),
        "the sidecar outlived the download"
    );

    // The row is still there, and it knows what it did.
    client.send(Command::List).await;
    let jobs = client
        .expect(|event| match event {
            Event::Jobs { jobs } => Some(jobs.clone()),
            _ => None,
        })
        .await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, JobState::Completed);
    assert_eq!(jobs[0].completed, SIZE);

    daemon.stop().await;
}

#[tokio::test]
async fn pause_keeps_the_bytes_and_resume_carries_on_from_them() {
    // Slow enough to still be running when the pause arrives.
    let server = hostile_server::spawn(SIZE, slow()).await;
    let root = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&root.path().join("data"), &root.path().join("downloads")).await;
    let mut client = daemon.client().await;

    let job = client
        .submit(JobSpec::file(envelope(server.url(), "paused.bin")))
        .await;
    client
        .send(Command::Subscribe {
            scope: SubscriptionScope::Detail { job },
        })
        .await;

    let paused_at = progress_past(&mut client, SIZE / 2).await;

    client.send(Command::Pause { job }).await;
    client
        .expect(harness::matching(|event| {
            matches!(event, Event::JobStateChanged { state: JobState::Paused, .. })
        }))
        .await;

    let part = root
        .path()
        .join("downloads")
        .join("paused.bin.vxpart");
    assert!(part.exists(), "pausing threw the partial data away");

    client.send(Command::Resume { job }).await;
    let after = progress_past(&mut client, 0).await.completed;
    assert!(
        after + in_flight_allowance(&paused_at) >= paused_at.completed,
        "resume came back at {after} bytes after {} were down — more than the          {} bytes that were legitimately still in flight",
        paused_at.completed,
        in_flight_allowance(&paused_at)
    );

    let (path, bytes) = completed(client.expect(finished(job)).await);
    assert_eq!(bytes, SIZE);
    assert_eq!(
        hostile_server::sha256_hex(&std::fs::read(&path).unwrap()),
        server.sha256
    );
    daemon.stop().await;
}

#[tokio::test]
async fn a_download_survives_the_daemon_being_stopped_and_started() {
    let server = hostile_server::spawn(SIZE, slow()).await;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let downloads = root.path().join("downloads");

    let endpoint = harness::unique_endpoint();
    let daemon = Daemon::start_at(&endpoint, &data, &downloads).await;
    let mut client = daemon.client().await;
    let job = client
        .submit(JobSpec::file(envelope(server.url(), "restarted.bin")))
        .await;
    client
        .send(Command::Subscribe {
            scope: SubscriptionScope::Detail { job },
        })
        .await;
    let before = progress_past(&mut client, SIZE / 2).await;

    // The daemon goes away mid-transfer. Nobody asked the user anything.
    drop(client);
    daemon.stop().await;
    assert!(downloads.join("restarted.bin.vxpart").exists());

    let daemon = Daemon::start_at(&endpoint, &data, &downloads).await;
    let mut client = daemon.client().await;
    client
        .send(Command::Subscribe {
            scope: SubscriptionScope::Detail { job },
        })
        .await;

    // The restored job is the same job, with the same id, and it picks up where it was.
    client.send(Command::List).await;
    let jobs = client
        .expect(|event| match event {
            Event::Jobs { jobs } => Some(jobs.clone()),
            _ => None,
        })
        .await;
    assert_eq!(jobs.len(), 1, "the list did not survive the restart");
    assert_eq!(jobs[0].id, job);
    assert!(
        jobs[0].completed + in_flight_allowance(&before) >= before.completed,
        "the restart lost more than the blocks that were in flight: {} of {}",
        jobs[0].completed,
        before.completed
    );
    // And the restored number is the durable one, not the last frame the UI happened to
    // see — progress after a restart goes forward, never backward.
    let restored = jobs[0].completed;
    assert!(progress_past(&mut client, 0).await.completed >= restored);

    let (path, bytes) = completed(client.expect(finished(job)).await);
    assert_eq!(bytes, SIZE);
    assert_eq!(
        hostile_server::sha256_hex(&std::fs::read(&path).unwrap()),
        server.sha256
    );
    daemon.stop().await;
}

#[tokio::test]
async fn an_expired_link_is_renewed_through_the_extension_and_the_transfer_continues() {
    let server = hostile_server::spawn(
        SIZE,
        Behaviour {
            expires_after: Some(Duration::from_millis(800)),
            ..slow()
        },
    )
    .await;
    let root = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&root.path().join("data"), &root.path().join("downloads")).await;
    let mut client = daemon.client().await;

    let job = client
        .submit(JobSpec::file(envelope(server.url(), "signed.bin")))
        .await;
    client
        .send(Command::Subscribe {
            scope: SubscriptionScope::Detail { job },
        })
        .await;

    // Get some of the file down, then stop while the signature is still good.
    let before = progress_past(&mut client, SIZE / 2).await;
    client.send(Command::Pause { job }).await;
    client
        .expect(harness::matching(|event| {
            matches!(event, Event::JobStateChanged { state: JobState::Paused, .. })
        }))
        .await;

    // The link dies while the job is parked — the laptop lid case.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    client.send(Command::Resume { job }).await;

    // Stand in for the extension: re-mint the URL in page context and hand it back. The
    // engine resumes on the bitmap it already has (02 §6).
    let mut renewals = 0;
    let mut resumed_at = None;
    let outcome = loop {
        match client.next().await {
            Event::UrlExpired { job: id, .. } if id == job => {
                renewals += 1;
                server.rotate_token();
                client
                    .send(Command::RenewedUrl {
                        job,
                        envelope: envelope(server.url(), "signed.bin"),
                    })
                    .await;
            }
            Event::JobProgress { frame, .. } if renewals > 0 && resumed_at.is_none() => {
                resumed_at = Some(frame.completed);
            }
            Event::JobFinished { job: id, outcome } if id == job => break outcome,
            _ => {}
        }
    };

    assert!(renewals > 0, "the link never expired — the test proves nothing");
    assert!(
        resumed_at.unwrap_or(0) + in_flight_allowance(&before) >= before.completed,
        "renewal restarted the download instead of resuming on the existing bitmap"
    );
    let (path, bytes) = completed(outcome);
    assert_eq!(bytes, SIZE);
    assert_eq!(
        hostile_server::sha256_hex(&std::fs::read(&path).unwrap()),
        server.sha256
    );
    daemon.stop().await;
}

#[tokio::test]
async fn cancel_takes_the_row_and_the_partial_data_with_it() {
    let server = hostile_server::spawn(SIZE, slow()).await;
    let root = tempfile::tempdir().unwrap();
    let downloads = root.path().join("downloads");
    let daemon = Daemon::start(&root.path().join("data"), &downloads).await;
    let mut client = daemon.client().await;

    let job = client
        .submit(JobSpec::file(envelope(server.url(), "cancelled.bin")))
        .await;
    client
        .send(Command::Subscribe {
            scope: SubscriptionScope::Detail { job },
        })
        .await;
    client
        .expect(harness::matching(|event| {
            matches!(event, Event::JobProgress { frame, .. } if frame.completed > 0)
        }))
        .await;

    client.send(Command::Cancel { job }).await;
    let outcome = client.expect(finished(job)).await;
    assert!(matches!(outcome, Outcome::Cancelled));
    client
        .expect(harness::matching(|event| {
            matches!(event, Event::JobRemoved { .. })
        }))
        .await;

    assert!(!downloads.join("cancelled.bin.vxpart").exists());
    assert!(!downloads.join("cancelled.bin.vxpart.meta").exists());
    assert!(!downloads.join("cancelled.bin").exists());

    client.send(Command::List).await;
    let jobs = client
        .expect(|event| match event {
            Event::Jobs { jobs } => Some(jobs.clone()),
            _ => None,
        })
        .await;
    assert!(jobs.is_empty(), "a cancelled job stayed in the list");
    daemon.stop().await;
}

#[tokio::test]
async fn a_client_speaking_a_different_protocol_is_told_so_plainly() {
    let root = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&root.path().join("data"), &root.path().join("downloads")).await;

    let mut client = Client::connect_raw(&daemon.endpoint).await;
    client
        .send(Command::Hello {
            client: "from the future".into(),
            protocol: vortex_proto::PROTOCOL_VERSION + 99,
        })
        .await;
    let message = client
        .expect(|event| match event {
            Event::Error { message } => Some(message.clone()),
            _ => None,
        })
        .await;
    assert!(message.contains("protocol"), "{message}");
    daemon.stop().await;
}

#[tokio::test]
async fn a_gone_file_fails_once_and_stays_failed() {
    let server = hostile_server::spawn(SIZE, Behaviour::default()).await;
    let root = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&root.path().join("data"), &root.path().join("downloads")).await;
    let mut client = daemon.client().await;

    let url = format!("http://{}/missing", server.addr);
    let job = client.submit(JobSpec::file(envelope(url, "gone.bin"))).await;

    let outcome = client.expect(finished(job)).await;
    match outcome {
        Outcome::Failed { error } => {
            assert!(!error.contains("404"), "a status code is not a sentence: {error}");
        }
        other => panic!("a missing file should fail: {other:?}"),
    }

    // And it stays failed rather than retrying forever in the background.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    client.send(Command::List).await;
    let jobs = client
        .expect(|event| match event {
            Event::Jobs { jobs } => Some(jobs.clone()),
            _ => None,
        })
        .await;
    assert!(matches!(jobs[0].state, JobState::Failed { .. }));
    daemon.stop().await;
}

#[tokio::test]
async fn credentials_stay_in_memory_and_never_reach_the_database() {
    let server = hostile_server::spawn(SIZE, Behaviour::default()).await;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let daemon = Daemon::start(&data, &root.path().join("downloads")).await;
    let mut client = daemon.client().await;

    let job = client
        .submit(JobSpec::file(envelope(server.url(), "secret.bin")))
        .await;
    let _ = client.expect(finished(job)).await;
    daemon.stop().await;

    let db = std::fs::read(data.join("vortex.db")).unwrap();
    let text = String::from_utf8_lossy(&db);
    assert!(!text.contains("session=secret"), "a cookie was written to vortex.db");
    // What is safe to keep is kept: the header that stops the resume 403ing.
    assert!(text.contains("Referer"));
}

#[tokio::test]
async fn an_idle_daemon_starts_and_stops_without_touching_anything() {
    let root = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&root.path().join("data"), &root.path().join("downloads")).await;
    let mut client = daemon.client().await;
    client.send(Command::Ping).await;
    client
        .expect(harness::matching(|event| matches!(event, Event::Pong)))
        .await;
    tokio::time::timeout(PATIENCE, daemon.stop())
        .await
        .expect("an idle daemon should stop immediately");
}
