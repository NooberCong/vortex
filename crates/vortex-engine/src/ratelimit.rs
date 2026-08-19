//! A shared token bucket for the global speed limit (05 §Settings).
//!
//! One bucket for the whole daemon, not one per job: the setting is "don't use more than
//! this much of my connection", which is a statement about the machine.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct RateLimiter {
    /// Bytes per second. 0 disables the limiter entirely — the common case, and it must
    /// cost nothing.
    limit: AtomicU64,
    state: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(bytes_per_second: u64) -> Self {
        Self {
            limit: AtomicU64::new(bytes_per_second),
            state: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn unlimited() -> Self {
        Self::new(0)
    }

    pub fn set_limit(&self, bytes_per_second: u64) {
        self.limit.store(bytes_per_second, Ordering::Relaxed);
    }

    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Waits until `bytes` may be consumed. Returns immediately when unlimited.
    pub async fn consume(&self, bytes: u64) {
        loop {
            let limit = self.limit.load(Ordering::Relaxed);
            if limit == 0 {
                return;
            }
            let wait = {
                let mut bucket = self.state.lock();
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
                bucket.last = now;
                // Cap the burst at one second's worth so a long idle period cannot release
                // a flood the moment a transfer starts.
                bucket.tokens = (bucket.tokens + elapsed * limit as f64).min(limit as f64);
                if bucket.tokens >= bytes as f64 {
                    bucket.tokens -= bytes as f64;
                    return;
                }
                let deficit = bytes as f64 - bucket.tokens;
                Duration::from_secs_f64((deficit / limit as f64).min(1.0))
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unlimited_bucket_never_waits() {
        let limiter = RateLimiter::unlimited();
        let start = Instant::now();
        for _ in 0..1000 {
            limiter.consume(1 << 20).await;
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn a_limited_bucket_paces_the_stream() {
        let limiter = RateLimiter::new(1_000_000);
        let start = Instant::now();
        // One second of burst is available immediately; the second megabyte must wait.
        limiter.consume(1_000_000).await;
        limiter.consume(1_000_000).await;
        assert!(
            start.elapsed() >= Duration::from_millis(500),
            "consumed 2 MB in {:?}",
            start.elapsed()
        );
    }
}
