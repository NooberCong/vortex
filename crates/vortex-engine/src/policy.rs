//! The per-origin policy cache (02 §1, §3, §6).
//!
//! Probe results are cached per **origin**, not per URL: range support, the learned
//! concurrency plateau, and the preferred ALPN. The second download from a host is optimal
//! from the first second.
//!
//! It also owns the circuit breakers, which are per-origin and not per-job — ten
//! consecutive transient failures against an origin pauses every job on it together,
//! instead of sixteen workers each hammering a struggling server.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use url::Url;
use vortex_proto::Protocol;

/// How long a learned ALPN preference is trusted.
const ALPN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// h3 is never load-bearing: any anomaly demotes the origin to h2 for a day, silently.
const H3_DEMOTION: Duration = Duration::from_secs(24 * 3600);
const BREAKER_THRESHOLD: u32 = 10;
const BREAKER_OPEN: Duration = Duration::from_secs(60);

/// What Vortex has learned about one origin. Persisted by the daemon, so it survives a
/// restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OriginPolicy {
    pub ranges: Option<bool>,
    /// The connection count the gradient controller settled on last time.
    pub plateau: Option<u8>,
    /// Set when the origin answered concurrency with 429s or a throughput drop.
    pub hostile_to_concurrency: bool,
    /// Set when h2 multiplexing behaved worse than plain h1 here.
    pub prefer_h11: bool,
    pub preferred_alpn: Option<PersistedProtocol>,
    /// Seconds since the epoch. `SystemTime` rather than `Instant` because this is written
    /// to disk and read back in another process lifetime.
    pub alpn_learned_at: Option<u64>,
    pub h3_demoted_until: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistedProtocol {
    Http11,
    H2,
    H3,
}

impl From<Protocol> for PersistedProtocol {
    fn from(p: Protocol) -> Self {
        match p {
            Protocol::Http11 => PersistedProtocol::Http11,
            Protocol::H2 => PersistedProtocol::H2,
            Protocol::H3 => PersistedProtocol::H3,
        }
    }
}

impl OriginPolicy {
    pub fn alpn_fresh(&self) -> Option<PersistedProtocol> {
        let learned = self.alpn_learned_at?;
        let age = now_secs().saturating_sub(learned);
        (age < ALPN_TTL.as_secs()).then_some(self.preferred_alpn).flatten()
    }

    pub fn h3_allowed(&self) -> bool {
        match self.h3_demoted_until {
            Some(until) => now_secs() >= until,
            None => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    /// Every job on this origin waits together.
    Open,
    /// One probe is allowed through.
    HalfOpen,
}

#[derive(Debug, Default)]
struct Breaker {
    consecutive: u32,
    opened_at: Option<Instant>,
    probing: bool,
}

#[derive(Default)]
pub struct PolicyCache {
    origins: Mutex<HashMap<String, OriginPolicy>>,
    breakers: Mutex<HashMap<String, Breaker>>,
}

impl PolicyCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scheme + host + port, which is the granularity range support and plateaus actually
    /// live at.
    pub fn origin_of(url: &str) -> String {
        match Url::parse(url) {
            Ok(u) => {
                let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
                match u.port() {
                    Some(port) => format!("{}://{host}:{port}", u.scheme()),
                    None => format!("{}://{host}", u.scheme()),
                }
            }
            Err(_) => url.to_owned(),
        }
    }

    pub fn get(&self, origin: &str) -> OriginPolicy {
        self.origins.lock().get(origin).cloned().unwrap_or_default()
    }

    pub fn update(&self, origin: &str, f: impl FnOnce(&mut OriginPolicy)) {
        let mut map = self.origins.lock();
        f(map.entry(origin.to_owned()).or_default());
    }

    pub fn snapshot(&self) -> Vec<(String, OriginPolicy)> {
        self.origins
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn restore(&self, entries: impl IntoIterator<Item = (String, OriginPolicy)>) {
        let mut map = self.origins.lock();
        for (origin, policy) in entries {
            map.insert(origin, policy);
        }
    }

    /// Records that h3 misbehaved here. Silent by design — the user should never see a
    /// protocol negotiation in the interface.
    pub fn demote_h3(&self, origin: &str) {
        self.update(origin, |p| {
            p.h3_demoted_until = Some(now_secs() + H3_DEMOTION.as_secs());
        });
    }

    pub fn record_success(&self, origin: &str) {
        let mut breakers = self.breakers.lock();
        let breaker = breakers.entry(origin.to_owned()).or_default();
        breaker.consecutive = 0;
        breaker.opened_at = None;
        breaker.probing = false;
    }

    /// Only transient failures count toward a breaker. A 404 is not the origin struggling.
    pub fn record_transient_failure(&self, origin: &str, now: Instant) {
        let mut breakers = self.breakers.lock();
        let breaker = breakers.entry(origin.to_owned()).or_default();
        breaker.consecutive += 1;
        breaker.probing = false;
        if breaker.consecutive >= BREAKER_THRESHOLD && breaker.opened_at.is_none() {
            breaker.opened_at = Some(now);
        }
    }

    pub fn breaker(&self, origin: &str, now: Instant) -> BreakerState {
        let mut breakers = self.breakers.lock();
        let Some(breaker) = breakers.get_mut(origin) else {
            return BreakerState::Closed;
        };
        match breaker.opened_at {
            None => BreakerState::Closed,
            Some(opened) if now.duration_since(opened) < BREAKER_OPEN => BreakerState::Open,
            Some(_) => {
                if breaker.probing {
                    BreakerState::Open
                } else {
                    breaker.probing = true;
                    BreakerState::HalfOpen
                }
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_ignore_the_path_and_the_query() {
        assert_eq!(
            PolicyCache::origin_of("https://cdn.example.com/a/b.iso?sig=1"),
            "https://cdn.example.com"
        );
        assert_eq!(
            PolicyCache::origin_of("http://Example.COM:8080/x"),
            "http://example.com:8080"
        );
    }

    #[test]
    fn ten_transient_failures_open_the_breaker_for_every_job_on_the_origin() {
        let cache = PolicyCache::new();
        let now = Instant::now();
        for _ in 0..9 {
            cache.record_transient_failure("https://x", now);
        }
        assert_eq!(cache.breaker("https://x", now), BreakerState::Closed);
        cache.record_transient_failure("https://x", now);
        assert_eq!(cache.breaker("https://x", now), BreakerState::Open);
    }

    #[test]
    fn the_breaker_half_opens_once_and_closes_on_success() {
        let cache = PolicyCache::new();
        let now = Instant::now();
        for _ in 0..10 {
            cache.record_transient_failure("https://x", now);
        }
        let later = now + BREAKER_OPEN + Duration::from_secs(1);
        assert_eq!(cache.breaker("https://x", later), BreakerState::HalfOpen);
        // Only one probe gets through.
        assert_eq!(cache.breaker("https://x", later), BreakerState::Open);
        cache.record_success("https://x");
        assert_eq!(cache.breaker("https://x", later), BreakerState::Closed);
    }

    #[test]
    fn h3_demotion_is_remembered_and_expires() {
        let cache = PolicyCache::new();
        assert!(cache.get("https://x").h3_allowed());
        cache.demote_h3("https://x");
        assert!(!cache.get("https://x").h3_allowed());

        cache.update("https://x", |p| {
            p.h3_demoted_until = Some(now_secs().saturating_sub(1))
        });
        assert!(cache.get("https://x").h3_allowed());
    }

    #[test]
    fn a_learned_plateau_survives_a_round_trip_through_persistence() {
        let cache = PolicyCache::new();
        cache.update("https://x", |p| {
            p.plateau = Some(12);
            p.ranges = Some(true);
        });
        let snapshot = cache.snapshot();

        let restored = PolicyCache::new();
        restored.restore(snapshot);
        assert_eq!(restored.get("https://x").plateau, Some(12));
        assert_eq!(restored.get("https://x").ranges, Some(true));
    }
}
