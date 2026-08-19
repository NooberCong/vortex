//! A server that misbehaves on demand (06 §Hostile-server suite).
//!
//! Every case here must produce a **correct file** or a **clean `NeedsDecision`**. None may
//! produce silent corruption. Speed is a feature; correctness under a lying server is the
//! product.
//!
//! The body is deterministic — byte *i* is a hash of *i* — so a response that serves the
//! wrong offset is detectable byte for byte, which is exactly what the liar check and the
//! per-worker `Content-Range` check exist to catch.

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use bytes::Bytes;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub mod media;

/// Byte *i* of the fixture. A splitmix step, so no two 16-byte windows collide in practice.
pub fn byte_at(i: u64) -> u8 {
    let mut x = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (x ^ (x >> 31)) as u8
}

pub fn fixture(len: u64) -> Vec<u8> {
    (0..len).map(byte_at).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Every way this server is allowed to lie. All default to honest.
#[derive(Debug, Clone, Default)]
pub struct Behaviour {
    /// Advertises `Accept-Ranges: bytes` and then ignores `Range`.
    pub advertise_ranges_ignore_them: bool,
    /// Returns 206 with a `Content-Range` that does not match the request.
    pub wrong_content_range: bool,
    /// Returns 206 whose body is taken from a different offset.
    pub wrong_body_offset: bool,
    /// Changes its `ETag` after the first response.
    pub etag_changes_midway: bool,
    /// Sends fewer bytes than `Content-Length` promises.
    pub truncate_short: bool,
    /// Claims `Content-Encoding: gzip` on a 206.
    pub gzip_206: bool,
    /// Returns a plausible body of zeros.
    pub zeros_body: bool,
    /// Drops the connection at 99% of every response.
    pub drop_at_99: bool,
    /// Returns 200 to an `If-Range` that should have produced 206.
    pub ignore_if_range: bool,
    /// Bytes per second, per connection — the classic IDM scenario.
    pub limit_rate: Option<u64>,
    /// Bytes per second for the *first* body request only, leaving every later connection
    /// at full speed. One slow edge among several fast ones, which is what makes a
    /// scheduler steal, evict and hedge — the three most intricate things it does.
    pub slow_first: Option<u64>,
    /// 429s past this many concurrent requests.
    pub max_concurrent: Option<usize>,
    /// Signed URLs stop working this long after the token was minted.
    pub expires_after: Option<Duration>,
    /// Serves no `Accept-Ranges` at all.
    pub no_ranges: bool,
    /// Offers a `Repr-Digest` the client can verify.
    pub offer_checksum: bool,
    /// Fails the first N requests with a 503.
    pub flaky_first: usize,
}

struct Fixture {
    /// What this server actually serves — zeros in the `zeros_body` fixture, the real
    /// object otherwise. Reference counted, so a 256 MiB fixture is generated once at
    /// startup and every range response is a slice of it rather than a copy. The
    /// throughput harness depends on that: an origin that regenerates its body per
    /// response is an origin whose cost scales with the client's connection count, which
    /// would hand a parallel client an advantage the network never gave it.
    body: Bytes,
    behaviour: Behaviour,
    requests: AtomicUsize,
    live: AtomicUsize,
    token: AtomicU64,
    /// Whether the one slow response has been handed out yet.
    slowed: std::sync::atomic::AtomicBool,
    /// When the current token was minted. Signed URLs expire relative to this.
    issued: std::sync::Mutex<Instant>,
    digest: String,
}

#[derive(Clone)]
pub struct Server {
    pub addr: SocketAddr,
    pub len: u64,
    pub sha256: String,
    state: Arc<Fixture>,
    shutdown: Arc<tokio::sync::Notify>,
}

impl Server {
    /// `/file` is the object; `/renew` mints a fresh signed URL, which is what the
    /// extension does in page context when `UrlExpired` arrives.
    pub fn url(&self) -> String {
        format!("http://{}/file?token={}", self.addr, self.current_token())
    }

    pub fn renew_url(&self) -> String {
        format!("http://{}/renew", self.addr)
    }

    pub fn current_token(&self) -> u64 {
        self.state.token.load(Ordering::SeqCst)
    }

    /// Issues a new token, as `/renew` does: older tokens stop working, and the new one
    /// starts its own lifetime. A rotation that did not reset the clock would hand back a
    /// URL that is already expired, which no real signing service does.
    pub fn rotate_token(&self) -> u64 {
        let token = self.state.token.fetch_add(1, Ordering::SeqCst) + 1;
        *self.state.issued.lock().unwrap() = Instant::now();
        token
    }

    pub fn requests(&self) -> usize {
        self.state.requests.load(Ordering::SeqCst)
    }

    /// Stops listening and lets the fixture body go. A test does not need this — the
    /// process is about to exit — but the throughput harness spawns one origin per fixture
    /// and would otherwise hold every fixture it has finished with in memory for the rest
    /// of the run.
    pub fn stop(&self) {
        self.shutdown.notify_one();
    }
}

/// Binds an ephemeral port on loopback and serves until the process exits.
pub async fn spawn(len: u64, behaviour: Behaviour) -> Server {
    // The honest object, once. `digest` and `sha256` always describe it even when the
    // server is about to serve zeros instead — a checksum that agreed with the lie would
    // make the `zeros_body` fixture untestable.
    let truth = Bytes::from(fixture(len));
    let digest = sha256_hex(&truth);
    let body = if behaviour.zeros_body {
        Bytes::from(vec![0u8; len as usize])
    } else {
        truth
    };
    let state = Arc::new(Fixture {
        body,
        behaviour,
        requests: AtomicUsize::new(0),
        live: AtomicUsize::new(0),
        token: AtomicU64::new(1),
        slowed: std::sync::atomic::AtomicBool::new(false),
        issued: std::sync::Mutex::new(Instant::now()),
        digest,
    });

    let app = Router::new()
        .route("/file", get(serve))
        .route("/renew", get(renew))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // `notify_one`, not `notify_waiters`: it leaves a permit behind, so a `stop()` that
    // beats the serve task to its first await still stops it.
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { signal.notified().await })
            .await;
    });

    Server {
        addr,
        len,
        sha256: state.digest.clone(),
        state,
        shutdown,
    }
}

/// What the extension does in page context when `UrlExpired` arrives: revisit the
/// originating endpoint and get a fresh signed URL for the same object.
async fn renew(State(state): State<Arc<Fixture>>) -> String {
    let token = state.token.fetch_add(1, Ordering::SeqCst) + 1;
    *state.issued.lock().unwrap() = Instant::now();
    token.to_string()
}

async fn serve(
    State(state): State<Arc<Fixture>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let b = &state.behaviour;
    let n = state.requests.fetch_add(1, Ordering::SeqCst);

    if n < b.flaky_first {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    if let Some(after) = b.expires_after {
        // The classic real-world failure: the signature elapses and the download dies at
        // 80%. A stale token is refused, and so is a current one past its lifetime.
        let presented = query
            .as_deref()
            .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")))
            .and_then(|t| t.parse::<u64>().ok());
        let expired = state.issued.lock().unwrap().elapsed() > after;
        if presented != Some(state.token.load(Ordering::SeqCst)) || expired {
            return status(StatusCode::FORBIDDEN);
        }
    }
    if let Some(max) = b.max_concurrent {
        if state.live.load(Ordering::SeqCst) >= max {
            return status(StatusCode::TOO_MANY_REQUESTS);
        }
    }
    state.live.fetch_add(1, Ordering::SeqCst);

    let total = state.body.len() as u64;
    let requested = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);

    let pretends_if_range_failed = b.ignore_if_range && headers.contains_key("if-range");
    let honour_range = requested.is_some()
        && !b.advertise_ranges_ignore_them
        && !b.no_ranges
        && !pretends_if_range_failed;

    let (start, end) = match (honour_range, requested) {
        (true, Some((s, e))) => (s.min(total), e.unwrap_or(total - 1).min(total - 1) + 1),
        _ => (0, total),
    };

    let mut response = Response::builder();
    if !b.no_ranges {
        response = response.header(header::ACCEPT_RANGES, "bytes");
    }
    let etag = if b.etag_changes_midway && n > 0 {
        "\"second\""
    } else {
        "\"first\""
    };
    response = response.header(header::ETAG, etag);
    response = response.header(header::CONTENT_TYPE, "application/octet-stream");
    if b.offer_checksum {
        use base64::Engine as _;
        let raw = hex_to_bytes(&state.digest);
        response = response.header(
            "repr-digest",
            format!(
                "sha-256=:{}:",
                base64::engine::general_purpose::STANDARD.encode(raw)
            ),
        );
    }
    if b.gzip_206 && honour_range {
        response = response.header(header::CONTENT_ENCODING, "gzip");
    }

    let promised = end - start;
    response = response.header(header::CONTENT_LENGTH, promised.to_string());

    let status_code = if honour_range {
        let claimed = if b.wrong_content_range {
            format!("bytes {}-{}/{total}", start + 4096, end + 4095)
        } else {
            format!("bytes {}-{}/{total}", start, end - 1)
        };
        response = response.header(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&claimed).unwrap(),
        );
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    let source_start = if b.wrong_body_offset && honour_range {
        (start + 65_536) % total
    } else {
        start
    };
    // Truncation, mid-body drops and the one slow edge only apply to real body requests.
    // Mangling a 16-byte probe would test the connect path, not the transfer path, and
    // every case here is about what happens to bytes that were supposed to become a file.
    const BODY_FLOOR: u64 = 1 << 20;
    let rate = match b.slow_first {
        Some(slow)
            if promised > BODY_FLOOR
                && state
                    .slowed
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok() =>
        {
            Some(slow)
        }
        _ => b.limit_rate,
    };
    let deliver = if b.truncate_short && promised > BODY_FLOOR {
        promised - 1024
    } else if b.drop_at_99 && promised > BODY_FLOOR {
        (promised as f64 * 0.99) as u64
    } else {
        promised
    };
    let abort = deliver < promised;

    let stream = body_stream(state.clone(), source_start, deliver, abort, rate);
    response
        .status(status_code)
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Decrements the live-connection count however the response ends — including when the
/// client hangs up mid-body, which a real download manager does constantly.
struct Live(Arc<Fixture>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

fn body_stream(
    state: Arc<Fixture>,
    start: u64,
    len: u64,
    abort_at_end: bool,
    rate: Option<u64>,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
    const CHUNK: u64 = 64 * 1024;
    let guard = Live(state.clone());
    futures::stream::unfold((0u64, guard), move |(sent, guard)| {
        let state = state.clone();
        async move {
            if sent >= len {
                return if abort_at_end && sent != u64::MAX {
                    // Cut the connection instead of ending the body cleanly.
                    Some((
                        Err(std::io::Error::other("connection reset by peer")),
                        (u64::MAX, guard),
                    ))
                } else {
                    None
                };
            }
            // One slice, never spanning the wrap. A chunk cut short at the seam is still a
            // legal chunk, and the alternative is stitching two pieces together for a case
            // only `wrong_body_offset` ever reaches.
            let total = state.body.len() as u64;
            let at = (start + sent) % total;
            let take = CHUNK.min(len - sent).min(total - at);
            if let Some(rate) = rate {
                tokio::time::sleep(Duration::from_secs_f64(take as f64 / rate as f64)).await;
            }
            let chunk = state.body.slice(at as usize..(at + take) as usize);
            Some((Ok(chunk), (sent + take, guard)))
        }
    })
}

fn status(code: StatusCode) -> Response {
    Response::builder()
        .status(code)
        .body(Body::empty())
        .unwrap()
}

fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    Some((
        start.trim().parse().ok()?,
        end.trim().parse::<u64>().ok(),
    ))
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect()
}
