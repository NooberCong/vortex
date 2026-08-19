//! Adaptive concurrency — gradient ascent, not a constant (02 §3).
//!
//! This is the mechanism that beats a fixed connection count *in both directions*:
//!
//! * against a per-connection-throttled host, throughput keeps climbing and the controller
//!   keeps adding — it finds 16 or 24 on its own;
//! * against an uncapped anycast CDN, the second connection yields nothing, the controller
//!   stops at 2, and Vortex adds no handshake overhead where there was nothing to win.
//!   That is how "never slower than Chrome" is *enforced* rather than asserted;
//! * against a host that punishes concurrency with 429s, it backs off and remembers.
//!
//! The learned plateau goes to the per-origin policy cache, so the second download from a
//! host is optimal from the first second.

use std::time::{Duration, Instant};

/// Where a job starts before it has any evidence.
///
/// Two, not four, and the difference is measured. The controller ticks every
/// [`INTERVAL`], so a transfer shorter than about two intervals never gets a decision at
/// all and runs at whatever it started on — and on a link a single stream already
/// saturates, every extra connection makes it *worse*. Against a real CDN over a real
/// link: 1 connection 2.18 s, 2 → 2.61 s, 4 → 3.06 s, 6 → 3.87 s for the same object.
/// Starting at four bet on parallelism before any evidence and lost 30% of the transfer
/// on the case the README promises to tie.
///
/// Two is the smallest number that can tell the difference — one connection can never
/// discover that a second would have helped — and the cost of starting low is bounded:
/// the climb is geometric, and the plateau goes to the per-origin cache, so a host that
/// rewards concurrency pays for the discovery once.
pub const START_CONNECTIONS: u8 = 2;
pub const HARD_CAP: u8 = 32;
/// Adding a connection must buy at least this much aggregate throughput to be kept.
const MIN_GAIN: f64 = 0.05;
/// A gain this close to proportional means the origin is shaping per connection and there
/// is no reason to walk up one at a time. Doubling reaches the ceiling in four steps
/// rather than fourteen, which is what makes starting low affordable.
const STRONG_GAIN: f64 = 0.5;
const INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add,
    Hold,
    /// Back off by this many connections and remember the origin punished us.
    Remove(u8),
}

#[derive(Debug, Clone)]
pub struct Controller {
    cap: u8,
    target: u8,
    /// Aggregate throughput measured at the previous tick.
    last_bps: f64,
    /// Connection count that produced `last_bps`.
    last_target: u8,
    last_tick: Option<Instant>,
    plateaued: bool,
    /// Set once the origin has pushed back; the learned ceiling is then sticky.
    punished: bool,
}

impl Controller {
    /// `cap` is `min(user_max, origin_plateau_from_cache, 32)` — a ceiling, not a target.
    pub fn new(cap: u8, start: u8) -> Self {
        let cap = cap.clamp(1, HARD_CAP);
        Self {
            cap,
            target: start.clamp(1, cap),
            last_bps: 0.0,
            last_target: 0,
            last_tick: None,
            plateaued: false,
            punished: false,
        }
    }

    pub fn target(&self) -> u8 {
        self.target
    }
    pub fn cap(&self) -> u8 {
        self.cap
    }
    /// The number worth writing to the policy cache once the job settles.
    pub fn learned_plateau(&self) -> Option<u8> {
        (self.plateaued || self.punished).then_some(self.target)
    }
    pub fn was_punished(&self) -> bool {
        self.punished
    }

    /// Call every tick with the aggregate EWMA throughput and whether any worker saw a
    /// 429/503/reset since the last call. Returns what to do about the connection count.
    pub fn tick(&mut self, bps: f64, pushback: bool, now: Instant) -> Action {
        if let Some(last) = self.last_tick {
            if now.duration_since(last) < INTERVAL {
                return Action::Hold;
            }
        }
        self.last_tick = Some(now);

        if pushback {
            self.punished = true;
            self.plateaued = true;
            // Halve. A server that is refusing connections is not asking to be probed one
            // at a time, and multiplicative decrease is the safe partner to an increase
            // that can double.
            let removed = (self.target / 2).min(self.target.saturating_sub(1));
            self.target -= removed;
            self.cap = self.cap.min(self.target.max(1));
            self.last_bps = bps;
            self.last_target = self.target;
            return if removed > 0 {
                Action::Remove(removed)
            } else {
                Action::Hold
            };
        }

        // First measurement: nothing to compare against yet, so take the same step the
        // no-comparison case takes below. If it turns out to have bought nothing, the next
        // tick gives it straight back.
        if self.last_target == 0 {
            self.last_bps = bps;
            self.last_target = self.target;
            return if self.target < self.cap {
                self.target = self.target.saturating_mul(2).min(self.cap);
                Action::Add
            } else {
                Action::Hold
            };
        }

        let gain = if self.last_bps > 0.0 {
            (bps - self.last_bps) / self.last_bps
        } else if bps > 0.0 {
            1.0
        } else {
            0.0
        };
        let previous = self.last_target;
        let grew = self.target > previous;
        self.last_bps = bps;
        self.last_target = self.target;

        if grew && gain < -MIN_GAIN {
            // Adding made it *worse* — the classic sign of a shaped or contended origin.
            self.punished = true;
            self.plateaued = true;
            let action = self.step_back_to(previous);
            self.cap = self.cap.min(self.target.max(1));
            return action;
        }

        if self.plateaued || self.target >= self.cap {
            return Action::Hold;
        }

        if !grew || gain > STRONG_GAIN {
            // Either there is nothing to compare against yet, or the origin is handing
            // back very nearly one connection's worth per connection.
            self.target = self.target.saturating_mul(2).min(self.cap);
            Action::Add
        } else if gain > MIN_GAIN {
            self.target += 1;
            Action::Add
        } else {
            // The step bought nothing. Give it back rather than keep it: those
            // connections are handshakes, sockets and server load in exchange for no
            // throughput at all, and on a saturated link they are worse than nothing.
            self.plateaued = true;
            self.step_back_to(previous)
        }
    }

    /// Returns to the count that produced the throughput being compared against, which is
    /// the only count on record as having earned itself.
    ///
    /// Deliberately leaves the ceiling alone. Finding the knee of a curve is not the same
    /// event as an origin refusing connections, and only the second one is a reason to
    /// remember a lower ceiling for the whole job.
    fn step_back_to(&mut self, previous: u8) -> Action {
        let removed = self.target.saturating_sub(previous.max(1));
        if removed == 0 {
            return Action::Hold;
        }
        self.target -= removed;
        Action::Remove(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticks() -> impl Iterator<Item = Instant> {
        let t0 = Instant::now();
        (1..).map(move |i| t0 + INTERVAL * i)
    }

    #[test]
    fn a_throttled_host_keeps_earning_more_connections() {
        let mut c = Controller::new(16, START_CONNECTIONS);
        let mut bps = 5_000_000.0;
        for now in ticks().take(20) {
            // Perfectly per-connection-throttled: throughput scales with the count.
            bps = 5_000_000.0 * c.target() as f64;
            c.tick(bps, false, now);
        }
        assert_eq!(c.target(), 16, "should have climbed to the ceiling");
        assert!(bps > 0.0);
    }

    #[test]
    fn an_uncapped_cdn_stops_at_two_and_adds_no_handshake_overhead() {
        let mut c = Controller::new(16, 1);
        // The link is already saturated: extra connections buy nothing at all.
        for now in ticks().take(10) {
            c.tick(900_000_000.0, false, now);
        }
        assert!(
            c.target() <= 2,
            "settled on {} connections against an uncapped origin",
            c.target()
        );
        assert_eq!(c.learned_plateau(), Some(c.target()));
    }

    #[test]
    fn pushback_halves_the_count_and_lowers_the_ceiling() {
        let mut c = Controller::new(32, 8);
        let mut it = ticks();
        assert_eq!(c.tick(1.0e8, true, it.next().unwrap()), Action::Remove(4));
        assert_eq!(c.target(), 4);
        assert!(c.was_punished());
        assert_eq!(c.cap(), 4, "the ceiling is now the learned number");
        assert_eq!(c.tick(1.0e8, false, it.next().unwrap()), Action::Hold);
    }

    #[test]
    fn throughput_dropping_after_a_step_gives_the_whole_step_back() {
        let mut c = Controller::new(16, 4);
        let mut it = ticks();
        c.tick(1.0e8, false, it.next().unwrap()); // baseline, then a doubling step → 8
        assert_eq!(c.target(), 8);
        // Worse than before the step: hand back exactly what the step took, not a
        // fixed two, or the count drifts away from the last number that earned itself.
        assert_eq!(c.tick(6.0e7, false, it.next().unwrap()), Action::Remove(4));
        assert_eq!(c.target(), 4);
    }

    #[test]
    fn a_step_that_bought_nothing_is_given_back_rather_than_kept() {
        // The case the module is named for and the one it used to get wrong: on a link a
        // single stream already saturates, the extra connections are pure cost, and
        // plateauing *on top of them* keeps paying it for the rest of the transfer.
        let mut c = Controller::new(16, START_CONNECTIONS);
        let mut it = ticks();
        c.tick(4.6e7, false, it.next().unwrap());
        assert!(c.target() > START_CONNECTIONS, "it should try");
        c.tick(4.6e7, false, it.next().unwrap());
        assert_eq!(c.target(), START_CONNECTIONS, "and give it back");
        assert_eq!(c.learned_plateau(), Some(START_CONNECTIONS));
    }

    #[test]
    fn the_controller_never_exceeds_the_hard_cap() {
        let mut c = Controller::new(200, 4);
        assert_eq!(c.cap(), HARD_CAP, "a user ceiling above 32 is clamped");
        // Throughput that keeps rewarding every added connection well past the ceiling.
        for now in ticks().take(60) {
            c.tick(1.0e6 * (c.target() as f64).powi(2), false, now);
        }
        assert_eq!(c.target(), HARD_CAP);
    }

    #[test]
    fn diminishing_returns_stop_the_climb_before_the_ceiling() {
        // A saturating origin: the first few connections buy a lot, the later ones buy
        // almost nothing. Where exactly it stops is arithmetic; that it stops short of
        // the ceiling, and records the number, is the property.
        let mut c = Controller::new(32, START_CONNECTIONS);
        for now in ticks().take(40) {
            let n = c.target() as f64;
            c.tick(1.0e8 * n / (n + 4.0), false, now);
        }
        assert!(
            c.target() < c.cap(),
            "climbed to {} of a {} ceiling on an origin that stopped paying",
            c.target(),
            c.cap()
        );
        assert_eq!(c.learned_plateau(), Some(c.target()));
    }

    #[test]
    fn ticks_closer_together_than_the_interval_are_ignored() {
        let mut c = Controller::new(16, 4);
        let t0 = Instant::now();
        c.tick(1.0e8, false, t0);
        let before = c.target();
        assert_eq!(c.tick(9.0e8, false, t0 + Duration::from_millis(100)), Action::Hold);
        assert_eq!(c.target(), before);
    }
}
