//! Live-network checks. Ignored by default — CI must not depend on someone else's CDN.
//!
//! Run with `cargo test -p vortex-engine --test real_network -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use vortex_engine::job::{self, Engine, TransferSpec};
use vortex_engine::transport::{Transport, TransportConfig};
use vortex_proto::RequestEnvelope;

/// A real HTTPS origin that redirects, speaks h2, and serves ranges.
const URL: &str = "https://speed.cloudflare.com/__down?bytes=33554432";

#[tokio::test]
#[ignore = "needs the network"]
async fn a_real_cdn_download_completes_and_reports_what_it_did() {
    let engine = Arc::new(Engine::new(Transport::new(TransportConfig {
        connect_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(30),
        ..Default::default()
    })));
    let dir = tempfile::tempdir().unwrap();

    let mut envelope = RequestEnvelope::new(URL);
    envelope.set_header(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36",
    );

    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let (_control, control_rx) = job::control();

    let done = job::transfer(
        engine,
        TransferSpec {
            envelope,
            dest_dir: dir.path().to_path_buf(),
            filename: Some("cdn-32mb.bin".into()),
            max_connections: 8,
            write_budget: 64 << 20,
        },
        tx,
        control_rx,
    )
    .await
    .expect("a real CDN download should complete");

    println!(
        "{} bytes in {:?} over {:?} across {} addresses, {} retries ({} recovered)",
        done.bytes, done.elapsed, done.protocol, done.addresses, done.retries.0, done.retries.1
    );
    assert_eq!(done.bytes, 33_554_432);
    assert_eq!(std::fs::metadata(&done.path).unwrap().len(), 33_554_432);
}
