//! Turning one live progress frame into what each subscriber actually needs (01 §IPC).
//!
//! An expanded row wants every worker's lease at 20 Hz. A collapsed row is a 6-pixel bar
//! at 2 Hz, and sending it forty worker runs would be sending it forty runs it cannot
//! draw. This is the difference between a 0.3% and a 9% idle CPU floor with 40 jobs.

use vortex_proto::{ProgressFrame, SummaryFrame};

/// How many runs a collapsed bar can usefully show. Six pixels of bar cannot resolve more.
const SUMMARY_RUNS: usize = 24;

pub fn summary(frame: &ProgressFrame) -> SummaryFrame {
    SummaryFrame {
        total: frame.total,
        completed: frame.completed,
        bps: frame.bps,
        eta_secs: frame.eta_secs,
        connections: frame.connections,
        blocks: frame.blocks,
        runs: coarsen(&frame.runs, SUMMARY_RUNS),
    }
}

/// Collapses the per-worker map to "done / in flight", then merges the smallest runs until
/// the frame is small enough to be worth sending twice a second.
fn coarsen(runs: &[(u32, u32, u8)], limit: usize) -> Vec<(u32, u32, u8)> {
    let mut out: Vec<(u32, u32, u8)> = Vec::with_capacity(runs.len().min(limit + 1));
    for &(start, len, owner) in runs {
        // The collapsed bar says "filled" or "moving"; which lane owns a block is a detail
        // only the expanded map has room for.
        let owner = u8::from(owner != 0);
        match out.last_mut() {
            Some(last) if last.2 == owner && last.0 + last.1 == start => last.1 += len,
            _ => out.push((start, len, owner)),
        }
    }
    while out.len() > limit {
        // Absorb the shortest run into its left neighbour — visually the shortest run is
        // the one whose disappearance is least noticeable.
        let victim = (1..out.len())
            .min_by_key(|&i| out[i].1)
            .expect("more than `limit` runs");
        let (_, len, _) = out.remove(victim);
        out[victim - 1].1 += len;
        // The merge may have made two neighbours agree; rejoin them.
        if victim < out.len() && out[victim - 1].2 == out[victim].2 {
            let (_, len, _) = out.remove(victim);
            out[victim - 1].1 += len;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_workers_become_one_moving_run() {
        let runs = vec![(0, 10, 0), (10, 5, 1), (15, 5, 2), (20, 5, 3)];
        assert_eq!(coarsen(&runs, 24), vec![(0, 10, 0), (10, 15, 1)]);
    }

    #[test]
    fn a_busy_map_is_cut_down_without_losing_or_inventing_blocks() {
        let runs: Vec<_> = (0..200)
            .map(|i| (i * 3, 3, (i % 2) as u8))
            .collect();
        let coarse = coarsen(&runs, 24);
        assert!(coarse.len() <= 24, "{} runs", coarse.len());
        assert_eq!(coarse[0].0, 0);
        let covered: u32 = coarse.iter().map(|r| r.1).sum();
        assert_eq!(covered, 600, "every block is still accounted for");
        for pair in coarse.windows(2) {
            assert_eq!(pair[0].0 + pair[0].1, pair[1].0, "runs stay contiguous");
        }
    }

    #[test]
    fn a_summary_carries_the_numbers_a_collapsed_row_shows() {
        let frame = ProgressFrame {
            total: Some(1 << 30),
            completed: 1 << 29,
            bps: 10_000_000,
            eta_secs: Some(53),
            connections: 8,
            block_size: 1 << 20,
            blocks: 1024,
            runs: (0..80).map(|i| (i * 12, 12, (i % 9) as u8)).collect(),
            workers: Vec::new(),
        };
        let summary = summary(&frame);
        assert_eq!(summary.completed, frame.completed);
        assert_eq!(summary.eta_secs, Some(53));
        assert!(summary.runs.len() <= SUMMARY_RUNS);
    }
}
