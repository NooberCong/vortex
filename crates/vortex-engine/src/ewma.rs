//! Exponentially weighted throughput, 3 s half-life.
//!
//! Smoothing happens here rather than in the UI, so every consumer — the scheduler's steal
//! arithmetic, the concurrency controller, and the number on screen — agrees. A raw
//! per-second readout flickering between 81 and 97 MB/s reads as instability even when the
//! transfer is perfectly steady (05 §Numbers).

use std::time::{Duration, Instant};

/// A gap longer than this means the machine slept or the job was paused. Decaying across
/// it would report a stall that never happened, so the estimate restarts instead.
const STALE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Ewma {
    bps: f64,
    tau: f64,
    last: Option<Instant>,
    /// Bytes seen since `last`, waiting for enough elapsed time to be a meaningful rate.
    carry: u64,
}

impl Ewma {
    pub fn new(half_life: Duration) -> Self {
        Self {
            bps: 0.0,
            tau: half_life.as_secs_f64() / std::f64::consts::LN_2,
            last: None,
            carry: 0,
        }
    }

    pub fn three_second() -> Self {
        Self::new(Duration::from_secs(3))
    }

    pub fn observe(&mut self, bytes: u64, now: Instant) {
        self.carry += bytes;
        let Some(last) = self.last else {
            self.last = Some(now);
            return;
        };
        let dt = now.saturating_duration_since(last);
        if dt > STALE_AFTER {
            self.bps = 0.0;
            self.carry = 0;
            self.last = Some(now);
            return;
        }
        // Below ~100 ms the sample is mostly scheduling noise; let it accumulate.
        if dt < Duration::from_millis(100) {
            return;
        }
        let dt = dt.as_secs_f64();
        let sample = self.carry as f64 / dt;
        let alpha = 1.0 - (-dt / self.tau).exp();
        self.bps += alpha * (sample - self.bps);
        self.carry = 0;
        self.last = Some(now);
    }

    /// Decays toward zero when nothing has arrived, so a dead worker reads as dead rather
    /// than holding its last good number forever.
    pub fn rate(&self, now: Instant) -> f64 {
        match self.last {
            Some(last) => {
                let idle = now.saturating_duration_since(last).as_secs_f64();
                if idle <= 0.0 {
                    self.bps
                } else {
                    self.bps * (-idle / self.tau).exp()
                }
            }
            None => 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.bps = 0.0;
        self.carry = 0;
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steady_stream_converges_on_its_true_rate() {
        let mut e = Ewma::three_second();
        let t0 = Instant::now();
        for i in 0..60 {
            e.observe(1_000_000, t0 + Duration::from_millis(500 * (i + 1)));
        }
        let rate = e.rate(t0 + Duration::from_millis(30_000));
        assert!(
            (rate - 2_000_000.0).abs() < 100_000.0,
            "converged to {rate} instead of 2 MB/s"
        );
    }

    #[test]
    fn a_worker_that_stops_decays_toward_zero() {
        let mut e = Ewma::three_second();
        let t0 = Instant::now();
        for i in 0..10 {
            e.observe(1_000_000, t0 + Duration::from_millis(500 * (i + 1)));
        }
        let live = e.rate(t0 + Duration::from_secs(5));
        let dead = e.rate(t0 + Duration::from_secs(25));
        assert!(dead < live / 10.0, "{dead} should be far below {live}");
    }

    #[test]
    fn sleeping_the_machine_invalidates_the_estimate_rather_than_faking_a_stall() {
        let mut e = Ewma::three_second();
        let t0 = Instant::now();
        e.observe(1_000_000, t0);
        e.observe(1_000_000, t0 + Duration::from_secs(1));
        assert!(e.rate(t0 + Duration::from_secs(1)) > 0.0);
        e.observe(0, t0 + Duration::from_secs(600));
        assert_eq!(e.rate(t0 + Duration::from_secs(600)), 0.0);
    }
}
