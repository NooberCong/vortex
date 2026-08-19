//! The scheduler (02 §3). The differentiator.
//!
//! Chrome splits statically into 3. aria2 splits at midpoints. Vortex does neither:
//! workers hold contiguous leases over a block bitmap, and when a worker runs out of work
//! it **steals from the lease with the largest predicted remaining time**, splitting at the
//! point that equalises the two predicted finish times:
//!
//! ```text
//! predicted_remaining(lease) = bytes_left(lease) / ewma_throughput(owner)
//! split_offset = cursor + bytes_left * ( r_new / (r_owner + r_new) )
//! ```
//!
//! Midpoint splitting hands half the remaining work to a worker that may be four times
//! slower, and the tail gets worse instead of better.
//!
//! This type is pure state: no sockets, no files, no async. That is what makes the parts
//! that matter — stealing, hedging, eviction — testable without a network.

use crate::blocks::{BlockMap, Range};
use crate::ewma::Ewma;
use parking_lot::Mutex;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vortex_proto::WorkerFrame;

pub type Lane = u8;

/// Below this, splitting costs more in handshakes than it saves.
pub const MIN_STEAL: u64 = 4 * 1024 * 1024;
/// Tail hedging starts here (02 §3).
const HEDGE_THRESHOLD: f64 = 0.95;
/// A worker slower than this fraction of the median, for this long, is replaced.
const SLOW_FRACTION: f64 = 0.2;
const SLOW_FOR: Duration = Duration::from_secs(5);
const SPARK_SAMPLES: usize = 6;

/// A worker's claim on a contiguous byte range. `end` is shared and shrinks when the
/// scheduler steals from it, so the running worker notices without a round trip.
#[derive(Debug, Clone)]
pub struct Lease {
    pub lane: Lane,
    pub start: u64,
    end: Arc<AtomicU64>,
}

impl Lease {
    pub fn end(&self) -> u64 {
        self.end.load(Ordering::Acquire)
    }
    /// True once the worker has fetched everything still assigned to it.
    pub fn finished(&self, cursor: u64) -> bool {
        cursor >= self.end()
    }
}

#[derive(Debug)]
struct LeaseState {
    start: u64,
    cursor: u64,
    end: Arc<AtomicU64>,
    /// `Some(victim)` when this lease is a duplicate racing a straggler.
    hedge_of: Option<Lane>,
    hedged: bool,
}

#[derive(Debug)]
struct WorkerStats {
    ewma: Ewma,
    spark: VecDeque<u32>,
    slow_since: Option<Instant>,
    stolen_from: bool,
}

impl Default for WorkerStats {
    fn default() -> Self {
        Self {
            ewma: Ewma::three_second(),
            spark: VecDeque::with_capacity(SPARK_SAMPLES),
            slow_since: None,
            stolen_from: false,
        }
    }
}

struct Inner {
    map: BlockMap,
    /// Unassigned byte ranges, largest first.
    pending: Vec<Range>,
    leases: BTreeMap<Lane, LeaseState>,
    workers: BTreeMap<Lane, WorkerStats>,
    /// Blocks completed but not yet journalled. Durability is the job's business; the
    /// scheduler only says what changed.
    journal: Vec<u32>,
}

pub struct Scheduler {
    inner: Mutex<Inner>,
}

impl Scheduler {
    pub fn new(map: BlockMap) -> Self {
        let pending = map.missing_ranges();
        Self {
            inner: Mutex::new(Inner {
                map,
                pending,
                leases: BTreeMap::new(),
                workers: BTreeMap::new(),
                journal: Vec::new(),
            }),
        }
    }

    /// Pre-splits the outstanding work into `n` roughly equal leases. Trivial, and
    /// immediately corrected by stealing — but it is what makes the file visibly shatter
    /// into workers in the first quarter-second (05 §The one orchestrated moment).
    pub fn seed(&self, n: u8) {
        let mut inner = self.inner.lock();
        let block = inner.map.block_size() as u64;
        let total_left: u64 = inner.pending.iter().map(|(s, e)| e - s).sum();
        if n <= 1 || total_left == 0 {
            return;
        }
        let target = (total_left / n as u64).max(block);
        let mut split = Vec::new();
        for (start, end) in std::mem::take(&mut inner.pending) {
            let mut cursor = start;
            while cursor < end {
                // At least one block forward so the loop always terminates, and never past
                // the end. Not `clamp`: almost every real file ends in a partial block, and
                // when the split lands a boundary on the last whole one, the floor
                // (`cursor + block`) is above the ceiling (`end`) and `clamp` panics — in
                // the daemon, taking every other download with it. Which boundaries occur
                // depends on the connection count, which is why this survived until the
                // starting count changed.
                let aligned = (cursor + target) / block * block;
                let next = aligned.max(cursor + block).min(end);
                split.push((cursor, next));
                cursor = next;
            }
        }
        inner.pending = split;
        inner.sort_pending();
    }

    /// Hands a lane work: an unassigned range if one exists, otherwise a slice stolen from
    /// whichever worker is predicted to finish last.
    pub fn acquire(&self, lane: Lane, now: Instant) -> Option<Lease> {
        let mut inner = self.inner.lock();
        inner.workers.entry(lane).or_default();

        if let Some((start, end)) = inner.pending.pop() {
            return Some(inner.install(lane, start, end, None));
        }
        inner.steal(lane, now)
    }

    /// Reports bytes fetched and the worker's new cursor. Returns the blocks that became
    /// complete, which the job journals only *after* the data is fsynced.
    pub fn progress(&self, lane: Lane, cursor: u64, bytes: u64, now: Instant) -> Vec<u32> {
        let mut inner = self.inner.lock();
        if let Some(stats) = inner.workers.get_mut(&lane) {
            stats.ewma.observe(bytes, now);
            let rate = stats.ewma.rate(now);
            if stats.spark.len() == SPARK_SAMPLES {
                stats.spark.pop_front();
            }
            stats.spark.push_back((rate / 1000.0) as u32);
        }

        let Some(lease) = inner.leases.get_mut(&lane) else {
            return Vec::new();
        };
        let previous = lease.cursor;
        lease.cursor = lease.cursor.max(cursor);
        let start = lease.start;
        let cursor = lease.cursor;

        let block = inner.map.block_size() as u64;
        let first = (previous.max(start) / block) as u32;
        let last = (cursor / block) as u32;
        let mut done = Vec::new();
        for index in first..=last.min(inner.map.blocks().saturating_sub(1)) {
            let (_, end) = inner.map.block_range(index);
            if cursor >= end && !inner.map.is_complete(index) {
                inner.map.mark(index);
                done.push(index);
            }
        }
        inner.journal.extend_from_slice(&done);
        done
    }

    /// A block a *hedge* finished on behalf of the straggler. Same bookkeeping, no lease.
    pub fn mark_block(&self, index: u32) {
        let mut inner = self.inner.lock();
        if !inner.map.is_complete(index) {
            inner.map.mark(index);
            inner.journal.push(index);
        }
    }

    pub fn is_block_complete(&self, index: u32) -> bool {
        self.inner.lock().map.is_complete(index)
    }

    /// Hands back everything the lane had not fetched. Called when a worker dies, is
    /// evicted, or the job pauses — its unfinished lease returns to the pool and a
    /// replacement opens against a different resolved IP.
    pub fn release(&self, lane: Lane) {
        let mut inner = self.inner.lock();
        let Some(lease) = inner.leases.remove(&lane) else {
            return;
        };
        let end = lease.end.load(Ordering::Acquire);
        let block = inner.map.block_size() as u64;
        // Round back to a block boundary: a partially fetched block is not durable, and
        // re-fetching a megabyte is cheaper than trusting a fraction of one.
        let resume_from = lease.cursor / block * block;
        if resume_from < end && lease.hedge_of.is_none() {
            inner.pending.push((resume_from, end));
            inner.sort_pending();
        }
        if let Some(victim) = lease.hedge_of {
            if let Some(v) = inner.leases.get_mut(&victim) {
                v.hedged = false;
            }
        }
    }

    /// Drops a lane's throughput history. Called when the worker itself goes away, not
    /// between leases — the steal arithmetic needs that history to survive a lease change.
    pub fn retire(&self, lane: Lane) {
        self.release(lane);
        self.inner.lock().workers.remove(&lane);
    }

    /// A server that ignored `Range` cannot be resumed mid-file. Forget everything and
    /// start over rather than stitch two halves together.
    pub fn reset(&self) {
        let mut inner = self.inner.lock();
        let total = inner.map.total();
        let block = inner.map.block_size();
        inner.map = BlockMap::new(total, block);
        inner.pending = inner.map.missing_ranges();
        inner.leases.clear();
        inner.journal.clear();
    }

    /// Blocks completed since the last call. The job fsyncs the data file first, then
    /// journals these — data before metadata, always.
    pub fn take_journal(&self) -> Vec<u32> {
        std::mem::take(&mut self.inner.lock().journal)
    }

    pub fn is_done(&self) -> bool {
        let inner = self.inner.lock();
        inner.map.is_done()
    }

    pub fn has_work(&self) -> bool {
        let inner = self.inner.lock();
        !inner.pending.is_empty() || !inner.leases.is_empty()
    }

    pub fn bitmap(&self) -> roaring::RoaringBitmap {
        self.inner.lock().map.bitmap().clone()
    }

    pub fn completed_bytes(&self) -> u64 {
        let inner = self.inner.lock();
        inner.completed_bytes()
    }

    pub fn active_lanes(&self) -> usize {
        self.inner.lock().leases.len()
    }

    /// A worker that landed on a bad edge node stays bad; replacing it is nearly always
    /// right. Returns the lane to cancel, if any.
    pub fn evict_candidate(&self, now: Instant) -> Option<Lane> {
        let mut inner = self.inner.lock();
        let rates: Vec<(Lane, f64)> = inner
            .leases
            .keys()
            .map(|lane| {
                let rate = inner
                    .workers
                    .get(lane)
                    .map(|w| w.ewma.rate(now))
                    .unwrap_or(0.0);
                (*lane, rate)
            })
            .collect();
        if rates.len() < 3 {
            return None;
        }
        let median = median_of(rates.iter().map(|(_, r)| *r));
        if median <= 0.0 {
            return None;
        }

        let mut evict = None;
        for (lane, rate) in rates {
            let slow = rate < median * SLOW_FRACTION;
            let Some(stats) = inner.workers.get_mut(&lane) else {
                continue;
            };
            match (slow, stats.slow_since) {
                (true, None) => stats.slow_since = Some(now),
                (true, Some(since)) if now.duration_since(since) >= SLOW_FOR => {
                    evict = Some(lane);
                }
                (false, _) => stats.slow_since = None,
                _ => {}
            }
        }
        evict
    }

    /// The last few percent is where wall-clock goes to die: fifteen workers idle while one
    /// straggler finishes. Returns a range worth fetching a second time on a fresh
    /// connection; first response to land wins.
    pub fn hedge_candidate(&self, now: Instant) -> Option<(Lane, Range)> {
        let mut inner = self.inner.lock();
        let total = inner.map.total();
        if total == 0 || (inner.completed_bytes() as f64 / total as f64) < HEDGE_THRESHOLD {
            return None;
        }
        if !inner.pending.is_empty() || inner.leases.len() < 2 {
            return None;
        }

        let mut predictions: Vec<(Lane, f64)> = Vec::new();
        for (lane, lease) in &inner.leases {
            if lease.hedge_of.is_some() || lease.hedged {
                continue;
            }
            let left = lease.end.load(Ordering::Acquire).saturating_sub(lease.cursor);
            if left == 0 {
                continue;
            }
            let rate = inner
                .workers
                .get(lane)
                .map(|w| w.ewma.rate(now))
                .unwrap_or(0.0);
            predictions.push((*lane, left as f64 / rate.max(1.0)));
        }
        if predictions.len() < 2 {
            return None;
        }
        predictions.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (lane, worst) = predictions[0];
        let others = percentile(predictions[1..].iter().map(|(_, p)| *p), 0.95);
        // Only when the straggler is genuinely out of line, and only when there is enough
        // time left for a second request to win.
        if worst < 1.0 || worst < others * 1.5 {
            return None;
        }

        let block = inner.map.block_size() as u64;
        let lease = inner.leases.get_mut(&lane)?;
        let start = lease.cursor / block * block;
        let end = lease.end.load(Ordering::Acquire);
        lease.hedged = true;
        Some((lane, (start, end)))
    }

    /// Installs a hedge lease racing `victim` over `range`.
    pub fn install_hedge(&self, lane: Lane, victim: Lane, range: Range) -> Lease {
        let mut inner = self.inner.lock();
        inner.workers.entry(lane).or_default();
        inner.install(lane, range.0, range.1, Some(victim))
    }

    /// Everything a `ProgressFrame` needs, computed under one lock.
    pub fn frame(&self, now: Instant) -> SchedulerFrame {
        let inner = self.inner.lock();
        let block = inner.map.block_size() as u64;
        let blocks = inner.map.blocks();

        let mut runs = inner.map.complete_runs();
        let mut workers = Vec::new();
        for (lane, lease) in &inner.leases {
            let end = lease.end.load(Ordering::Acquire);
            let first = (lease.cursor / block) as u32;
            let last = end.div_ceil(block) as u32;
            if last > first {
                runs.push((first, (last - first).min(blocks - first), *lane));
            }
            if let Some(stats) = inner.workers.get(lane) {
                workers.push(WorkerFrame {
                    lane: *lane,
                    bps: stats.ewma.rate(now) as u64,
                    spark: stats.spark.iter().copied().collect(),
                    stealing_from: stats.stolen_from,
                });
            }
        }
        runs.sort_by_key(|r| (r.0, r.2));

        SchedulerFrame {
            completed: inner.completed_bytes(),
            total: inner.map.total(),
            block_size: inner.map.block_size(),
            blocks,
            runs,
            workers,
            connections: inner.leases.len() as u8,
        }
    }
}

pub struct SchedulerFrame {
    pub completed: u64,
    pub total: u64,
    pub block_size: u32,
    pub blocks: u32,
    pub runs: Vec<(u32, u32, u8)>,
    pub workers: Vec<WorkerFrame>,
    pub connections: u8,
}

impl Inner {
    fn sort_pending(&mut self) {
        // Largest last, so `pop` hands out the biggest remaining chunk first.
        self.pending.sort_by_key(|(s, e)| e - s);
    }

    fn install(&mut self, lane: Lane, start: u64, end: u64, hedge_of: Option<Lane>) -> Lease {
        let shared = Arc::new(AtomicU64::new(end));
        self.leases.insert(
            lane,
            LeaseState {
                start,
                cursor: start,
                end: shared.clone(),
                hedge_of,
                hedged: false,
            },
        );
        Lease {
            lane,
            start,
            end: shared,
        }
    }

    /// Exact: completed blocks plus the partial head of each in-flight lease. Cannot
    /// overstate progress, which matters because the segment map must never round a
    /// partial block up to complete.
    fn completed_bytes(&self) -> u64 {
        let block = self.map.block_size() as u64;
        let mut bytes = self.map.completed_bytes();
        for lease in self.leases.values() {
            if lease.hedge_of.is_some() {
                continue;
            }
            let index = (lease.cursor / block) as u32;
            if index < self.map.blocks() && !self.map.is_complete(index) {
                bytes += lease.cursor - index as u64 * block;
            }
        }
        bytes.min(self.map.total())
    }

    fn steal(&mut self, lane: Lane, now: Instant) -> Option<Lease> {
        let block = self.map.block_size() as u64;
        let rates: BTreeMap<Lane, f64> = self
            .workers
            .iter()
            .map(|(l, w)| (*l, w.ewma.rate(now)))
            .collect();

        // The victim is the lease predicted to finish last — not the one with the most
        // bytes left.
        let mut best: Option<(Lane, f64, u64, u64)> = None;
        for (victim, lease) in &self.leases {
            if *victim == lane || lease.hedge_of.is_some() {
                continue;
            }
            let end = lease.end.load(Ordering::Acquire);
            let left = end.saturating_sub(lease.cursor);
            if left < MIN_STEAL * 2 {
                continue;
            }
            let rate = rates.get(victim).copied().unwrap_or(0.0).max(1.0);
            let predicted = left as f64 / rate;
            if best.is_none_or(|(_, p, _, _)| predicted > p) {
                best = Some((*victim, predicted, lease.cursor, end));
            }
        }
        let (victim, _, cursor, end) = best?;

        let r_owner = rates.get(&victim).copied().unwrap_or(0.0).max(1.0);
        // A brand-new lane has no history. Assume it will do as well as the median worker,
        // which splits evenly when everyone is equal and favours the idle worker when the
        // victim is the slow one.
        let r_new = match rates.get(&lane).copied().unwrap_or(0.0) {
            r if r > 0.0 => r,
            _ => median_of(rates.values().copied().filter(|r| *r > 0.0)).max(r_owner),
        };

        let left = end - cursor;
        let share = (left as f64 * (r_new / (r_owner + r_new))) as u64;
        // Round to a block boundary and keep both halves worth having.
        let mut split = end - share;
        split = split / block * block;
        split = split.clamp(cursor + MIN_STEAL, end.saturating_sub(MIN_STEAL));
        split = split / block * block;
        if split <= cursor || split >= end {
            return None;
        }

        self.leases
            .get_mut(&victim)
            .expect("victim lease was just read")
            .end
            .store(split, Ordering::Release);
        if let Some(stats) = self.workers.get_mut(&victim) {
            stats.stolen_from = true;
        }
        Some(self.install(lane, split, end, None))
    }
}

fn median_of(values: impl Iterator<Item = f64>) -> f64 {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Nearest-rank percentile. With a handful of workers p95 is the maximum, which is the
/// intent: a hedge is only worth issuing against a worker worse than all the others.
fn percentile(values: impl Iterator<Item = f64>, p: f64) -> f64 {
    let mut v: Vec<f64> = values.collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let rank = ((p * v.len() as f64).ceil() as usize).clamp(1, v.len());
    v[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::concurrency::HARD_CAP;

    const BLOCK: u32 = 1 << 20;

    fn scheduler(total: u64) -> Scheduler {
        Scheduler::new(BlockMap::new(total, BLOCK))
    }

    #[test]
    fn the_first_worker_gets_the_whole_file_and_the_second_steals_half_of_it() {
        let sched = scheduler(100 << 20);
        let now = Instant::now();
        let a = sched.acquire(1, now).unwrap();
        assert_eq!((a.start, a.end()), (0, 100 << 20));

        let b = sched.acquire(2, now).unwrap();
        // Neither has any history, so the split is even.
        assert_eq!(a.end(), 50 << 20);
        assert_eq!((b.start, b.end()), (50 << 20, 100 << 20));
    }

    #[test]
    fn a_fast_idle_worker_takes_a_bigger_share_from_a_slow_one() {
        let sched = scheduler(100 << 20);
        let t0 = Instant::now();
        let slow = sched.acquire(1, t0).unwrap();

        // Lane 1 is measured slow; lane 2 has been measured fast on earlier work.
        let fast = sched.acquire(2, t0).unwrap();
        for i in 1..20u32 {
            let now = t0 + Duration::from_millis(500 * i as u64);
            sched.progress(1, slow.start + (i as u64 * 100_000), 100_000, now);
            sched.progress(2, fast.start + (i as u64 * 4_000_000), 4_000_000, now);
        }
        let now = t0 + Duration::from_secs(10);
        sched.release(2);

        let slow_end_before = slow.end();
        let stealer = sched.acquire(2, now).unwrap();
        let taken = slow_end_before - slow.end();
        let kept = slow.end() - 4_000_000; // roughly what lane 1 had left
        assert!(
            taken > kept,
            "a fast worker should take the larger share: took {taken}, left {kept}"
        );
        assert_eq!(stealer.start, slow.end());
    }

    #[test]
    fn stealing_stops_when_the_remainder_is_not_worth_splitting() {
        let sched = scheduler(6 << 20);
        let now = Instant::now();
        sched.acquire(1, now).unwrap();
        // 6 MiB left cannot yield two 4 MiB halves.
        assert!(sched.acquire(2, now).is_none());
    }

    #[test]
    fn blocks_complete_only_once_the_cursor_has_passed_their_end() {
        let sched = scheduler(4 << 20);
        let now = Instant::now();
        let lease = sched.acquire(1, now).unwrap();
        assert_eq!(lease.start, 0);

        assert!(sched.progress(1, 500_000, 500_000, now).is_empty());
        assert_eq!(sched.progress(1, 1 << 20, 548_576, now), vec![0]);
        assert_eq!(sched.progress(1, 3 << 20, 2 << 20, now), vec![1, 2]);
        assert_eq!(sched.completed_bytes(), 3 << 20);
    }

    #[test]
    fn a_released_lease_returns_to_the_pool_on_a_block_boundary() {
        let sched = scheduler(64 << 20);
        let now = Instant::now();
        sched.acquire(1, now).unwrap();
        sched.progress(1, 5_500_000, 5_500_000, now);
        sched.release(1);

        let next = sched.acquire(2, now).unwrap();
        assert_eq!(next.start, 5 << 20, "resumes from the last whole block");
        assert_eq!(next.end(), 64 << 20);
    }

    #[test]
    fn seeding_splits_the_file_into_equal_leases() {
        let sched = scheduler(64 << 20);
        sched.seed(4);
        let now = Instant::now();
        let leases: Vec<_> = (1..=4).map(|l| sched.acquire(l, now).unwrap()).collect();
        let covered: u64 = leases.iter().map(|l| l.end() - l.start).sum();
        assert_eq!(covered, 64 << 20);
        assert_eq!(leases.len(), 4);
    }

    #[test]
    fn resume_leases_only_the_missing_ranges() {
        let mut map = BlockMap::new(64 << 20, BLOCK);
        map.mark_range(0, 32);
        let sched = Scheduler::new(map);
        let now = Instant::now();
        let lease = sched.acquire(1, now).unwrap();
        assert_eq!((lease.start, lease.end()), (32 << 20, 64 << 20));
        assert_eq!(sched.completed_bytes(), 32 << 20);
    }

    #[test]
    fn a_persistently_slow_worker_is_evicted_but_not_before_five_seconds() {
        let sched = scheduler(300 << 20);
        let t0 = Instant::now();
        for lane in 1..=3 {
            sched.acquire(lane, t0).unwrap();
        }
        let mut cursors = [0u64; 4];
        let mut now = t0;
        for step in 1..30u32 {
            now = t0 + Duration::from_millis(500 * step as u64);
            for lane in 1..=3u8 {
                let bytes = if lane == 3 { 10_000 } else { 4_000_000 };
                cursors[lane as usize] += bytes;
                sched.progress(lane, cursors[lane as usize], bytes, now);
            }
            if step == 4 {
                assert_eq!(sched.evict_candidate(now), None, "not yet — needs 5 s");
            }
        }
        assert_eq!(sched.evict_candidate(now), Some(3));
    }

    #[test]
    fn hedging_only_starts_in_the_last_five_percent_and_only_against_a_straggler() {
        // A fresh 400 MiB job is nowhere near the tail.
        let early = scheduler(400 << 20);
        early.seed(2);
        let t0 = Instant::now();
        early.acquire(1, t0).unwrap();
        early.acquire(2, t0).unwrap();
        assert_eq!(early.hedge_candidate(t0), None, "too early to hedge");

        // The same job with 95% already on disk and two workers on the remainder.
        let mut map = BlockMap::new(400 << 20, BLOCK);
        map.mark_range(0, 380);
        let sched = Scheduler::new(map);
        sched.seed(2);
        let a = sched.acquire(1, t0).unwrap();
        let b = sched.acquire(2, t0).unwrap();

        let (mut ca, mut cb) = (a.start, b.start);
        let mut now = t0;
        for step in 1..6u32 {
            now = t0 + Duration::from_millis(500 * step as u64);
            ca += 2_000_000;
            cb += 50_000;
            sched.progress(1, ca, 2_000_000, now);
            sched.progress(2, cb, 50_000, now);
        }

        let (victim, range) = sched.hedge_candidate(now).expect("straggler should be hedged");
        assert_eq!(victim, 2);
        assert!(range.0 <= cb && range.1 == b.end());
        assert_eq!(
            sched.hedge_candidate(now),
            None,
            "one hedge per straggler at a time"
        );
    }

    #[test]
    fn seeding_covers_the_file_exactly_whatever_the_connection_count() {
        // 81,942,945 bytes is a real object: 78 whole 1 MiB blocks and a 150 KiB
        // remainder. Split in two, a lease boundary landed on the last whole block and the
        // seed panicked. Split in four, it did not — so this sweeps both axes rather than
        // pinning the one pair that happened to break.
        for total in [
            81_942_945u64,
            BLOCK as u64 + 1,
            BLOCK as u64 - 1,
            4 * BLOCK as u64,
            (400 << 20) + 3,
        ] {
            for n in 1..=HARD_CAP {
                let sched = Scheduler::new(BlockMap::new(total, BLOCK));
                sched.seed(n);
                let mut ranges: Vec<(u64, u64)> = sched.inner.lock().pending.clone();
                ranges.sort_unstable();
                let mut at = 0u64;
                for (start, end) in &ranges {
                    assert_eq!(*start, at, "a gap or an overlap at {at} (total {total}, n {n})");
                    assert!(end > start, "an empty lease at {start} (total {total}, n {n})");
                    at = *end;
                }
                assert_eq!(at, total, "seeding lost the tail (total {total}, n {n})");
            }
        }
    }

    #[test]
    fn the_frame_never_claims_more_than_is_actually_there() {
        let sched = scheduler(10 << 20);
        let now = Instant::now();
        sched.acquire(1, now).unwrap();
        sched.progress(1, 1_500_000, 1_500_000, now);
        let frame = sched.frame(now);
        assert!(frame.completed < 2 << 20);
        assert_eq!(frame.connections, 1);
        assert_eq!(frame.workers.len(), 1);
        // One in-flight run owned by lane 1, no complete runs yet beyond block 0.
        assert!(frame.runs.iter().any(|(_, _, owner)| *owner == 1));
    }
}
