//! Median and tails.
//!
//! Five samples is what 06 asks for, and at five samples p10 and p90 are barely inside the
//! extremes — which is the point of printing them. A speedup of 3.1× whose p10 is 1.2× is
//! not a 3.1× speedup, it is a bimodal result with a story behind it, and a lone median
//! hides that.

pub struct Summary {
    pub median: f64,
    pub p10: f64,
    pub p90: f64,
}

pub fn summarize(samples: &[f64]) -> Summary {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a measured duration"));
    Summary {
        median: percentile(&sorted, 0.50),
        p10: percentile(&sorted, 0.10),
        p90: percentile(&sorted, 0.90),
    }
}

/// Linear interpolation between the two nearest ranks — the definition Excel, NumPy and
/// most people mean when they say "p90".
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = p * (sorted.len() - 1) as f64;
    let below = rank.floor() as usize;
    let above = rank.ceil() as usize;
    let lo = sorted[below];
    lo + (sorted[above] - lo) * (rank - below as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_sample_is_its_own_median_and_both_tails() {
        let s = summarize(&[2.0]);
        assert_eq!((s.p10, s.median, s.p90), (2.0, 2.0, 2.0));
    }

    #[test]
    fn the_median_of_an_even_count_falls_between_the_middle_two() {
        assert_eq!(summarize(&[1.0, 2.0, 3.0, 4.0]).median, 2.5);
    }

    #[test]
    fn the_tails_interpolate_rather_than_snapping_to_a_sample() {
        // Ranks 0.4 and 3.6 of [10, 20, 30, 40, 50] — 0.10 and 0.90 of four intervals.
        let s = summarize(&[30.0, 10.0, 50.0, 20.0, 40.0]);
        assert!((s.p10 - 14.0).abs() < 1e-9, "p10 was {}", s.p10);
        assert!((s.p90 - 46.0).abs() < 1e-9, "p90 was {}", s.p90);
        assert_eq!(s.median, 30.0);
    }
}
