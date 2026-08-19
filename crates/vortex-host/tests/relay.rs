//! The relay, end to end: a real daemon, the real `vortex-host` binary, and messages in
//! exactly the shape a WebExtension sends them.
//!
//! This is the test that catches a protocol drift the type system cannot: the extension
//! speaks JSON, the daemon speaks MessagePack, and the tag names have to agree.

use serde_json::json;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};

const PATIENCE: Duration = Duration::from_secs(15);

/// One test, not two: the host always talks to the one per-user endpoint — there is no way
/// to point it somewhere else, and that is the point — so two tests would be two daemons
/// fighting over the same address.
#[tokio::test]
async fn the_extension_talks_to_the_daemon_through_the_host() {
    // If a real daemon owns this user's endpoint, this machine cannot run the test.
    if vortex_ipc::is_running().await {
        eprintln!("a daemon already owns this user's endpoint; skipping");
        return;
    }

    with_no_daemon_the_host_says_so_and_closes_the_port().await;

    let data = tempfile::tempdir().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(vortexd::run_until(
        vortexd::Config {
            data_dir: data.path().to_path_buf(),
            endpoint: vortex_ipc::endpoint().unwrap(),
            ephemeral: true,
            ..Default::default()
        },
        async {
            let _ = stopped.await;
        },
    ));
    wait_for_daemon().await;

    let mut host = tokio::process::Command::new(env!("CARGO_BIN_EXE_vortex-host"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("starting vortex-host");
    let mut to_host = host.stdin.take().unwrap();
    let mut from_host = host.stdout.take().unwrap();

    // The heartbeat the extension uses before it dares cancel a browser download (03 §2).
    send(&mut to_host, &json!({ "cmd": "ping" })).await;
    let pong = receive(&mut from_host).await;
    assert_eq!(pong["event"], "pong", "{pong}");

    send(
        &mut to_host,
        &json!({
            "cmd": "hello",
            "client": "vortex-extension",
            "protocol": vortex_proto::PROTOCOL_VERSION,
        }),
    )
    .await;
    let hello = receive(&mut from_host).await;
    assert_eq!(hello["event"], "hello", "{hello}");

    // A submitted envelope, in the shape the request ledger builds (03 §1).
    send(
        &mut to_host,
        &json!({
            "cmd": "submit",
            "spec": {
                "envelope": {
                    "url": "https://example.invalid/never-resolves.bin",
                    "method": "GET",
                    "headers": [["User-Agent", "Mozilla/5.0"], ["Referer", "https://example.invalid/"]],
                    "cookies": "session=secret",
                    "capturedAt": 0
                },
                "priority": "Normal",
                "startPaused": true
            }
        }),
    )
    .await;
    let added = receive(&mut from_host).await;
    assert_eq!(added["event"], "jobAdded", "{added}");
    assert_eq!(added["job"]["filename"], "never-resolves.bin", "{added}");
    assert_eq!(added["job"]["state"]["kind"], "paused", "{added}");

    send(&mut to_host, &json!({ "cmd": "list" })).await;
    let jobs = receive(&mut from_host).await;
    assert_eq!(jobs["event"], "jobs", "{jobs}");
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 1, "{jobs}");

    shutdown(host, to_host).await;
    let _ = stop.send(());
    let _ = tokio::time::timeout(PATIENCE, daemon).await;
}

/// No daemon, and — because the host is copied somewhere on its own first — no `vortexd`
/// beside it to start. So it has to give up. The extension reads a closed port as
/// `Disconnected` and stops capturing rather than cancelling a download it cannot hand
/// over (03 §2).
async fn with_no_daemon_the_host_says_so_and_closes_the_port() {
    let alone = tempfile::tempdir().unwrap();
    let host_exe = alone.path().join(if cfg!(windows) {
        "vortex-host.exe"
    } else {
        "vortex-host"
    });
    std::fs::copy(env!("CARGO_BIN_EXE_vortex-host"), &host_exe).unwrap();

    let mut host = tokio::process::Command::new(&host_exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("starting vortex-host");
    let mut from_host = host.stdout.take().unwrap();

    let message = tokio::time::timeout(PATIENCE, receive(&mut from_host))
        .await
        .expect("the host neither answered nor exited");
    assert_eq!(message["event"], "error", "{message}");

    let closed = tokio::time::timeout(PATIENCE, host.wait()).await;
    assert!(closed.is_ok(), "the host stayed open with nothing to relay");
}

async fn wait_for_daemon() {
    for _ in 0..200 {
        if vortex_ipc::is_running().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the daemon never came up");
}

async fn send(writer: &mut ChildStdin, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).unwrap();
    writer
        .write_all(&(body.len() as u32).to_ne_bytes())
        .await
        .unwrap();
    writer.write_all(&body).await.unwrap();
    writer.flush().await.unwrap();
}

async fn receive(reader: &mut ChildStdout) -> serde_json::Value {
    let mut len = [0u8; 4];
    tokio::time::timeout(PATIENCE, reader.read_exact(&mut len))
        .await
        .expect("the host went quiet")
        .expect("reading a length prefix");
    let mut body = vec![0u8; u32::from_ne_bytes(len) as usize];
    reader.read_exact(&mut body).await.expect("reading a message");
    serde_json::from_slice(&body).expect("the host sent something that is not JSON")
}

async fn shutdown(mut host: Child, to_host: ChildStdin) {
    // Closing stdin is how the browser ends a native messaging session, and the host is
    // expected to notice and leave.
    drop(to_host);
    let _ = tokio::time::timeout(PATIENCE, host.wait()).await;
    let _ = host.kill().await;
}
