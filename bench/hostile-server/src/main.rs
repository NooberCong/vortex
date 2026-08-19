//! Runs one hostile fixture on a fixed port, for manual poking and for the throughput
//! harness. The engine's own test suite calls the library directly.

use clap::Parser;
use hostile_server::Behaviour;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "hostile-server", about = "A server that misbehaves on demand")]
struct Args {
    /// Fixture size in bytes.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    size: u64,
    /// Per-connection rate limit in bytes per second — the `nginx limit_rate` fixture.
    #[arg(long)]
    limit_rate: Option<u64>,
    /// 429 past this many concurrent connections.
    #[arg(long)]
    max_concurrent: Option<usize>,
    /// Signed URLs expire this many seconds after start.
    #[arg(long)]
    expires_after: Option<u64>,
    #[arg(long)]
    no_ranges: bool,
    #[arg(long)]
    wrong_content_range: bool,
    #[arg(long)]
    wrong_body_offset: bool,
    #[arg(long)]
    truncate_short: bool,
    #[arg(long)]
    drop_at_99: bool,
    #[arg(long)]
    etag_changes_midway: bool,
    #[arg(long)]
    gzip_206: bool,
    #[arg(long)]
    offer_checksum: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let server = hostile_server::spawn(
        args.size,
        Behaviour {
            limit_rate: args.limit_rate,
            max_concurrent: args.max_concurrent,
            expires_after: args.expires_after.map(Duration::from_secs),
            no_ranges: args.no_ranges,
            wrong_content_range: args.wrong_content_range,
            wrong_body_offset: args.wrong_body_offset,
            truncate_short: args.truncate_short,
            drop_at_99: args.drop_at_99,
            etag_changes_midway: args.etag_changes_midway,
            gzip_206: args.gzip_206,
            offer_checksum: args.offer_checksum,
            ..Default::default()
        },
    )
    .await;

    println!("{}", server.url());
    println!("sha256 {}", server.sha256);
    std::future::pending::<()>().await;
}
