//! Number formatting, defined once so the CLI, the daemon logs and the UI never disagree
//! (05 §"Numbers that don't lie or twitch"). The TypeScript mirror lives in
//! `packages/proto/src/format.ts` — beside the generated types, shared by the desktop app
//! and the page overlay — and its suite runs the case tables below verbatim.
//!
//! Sizes are displayed base-10 (MB, GB) to match the file manager and the source page,
//! and computed base-2 internally. Documented, consistent, never mixed.

/// Base-10 size, three significant figures. `1_290_000_000` renders `1.29 GB`, never
/// `1.2 GB` — showing 1.2 for a 1.29 file is the kind of small lie users notice.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{} {}", three_sig_figs(value), UNITS[unit])
}

/// Throughput. Same scale as [`bytes`], with `/s`.
pub fn rate(bps: u64) -> String {
    if bps == 0 {
        return "—".to_owned();
    }
    format!("{}/s", bytes(bps))
}

/// Quantized so it never changes every frame: `< 60s` seconds, `< 60m` as `4m 20s`,
/// above that `1h 12m`. Never `3,847 seconds`.
pub fn eta(secs: u32) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => {
            let (m, s) = (secs / 60, secs % 60);
            if s == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {s}s")
            }
        }
        _ => {
            let (h, m) = (secs / 3600, (secs % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
    }
}

/// No fake precision: `62%`, not `62.4%`. Rounds toward zero so a job never reads 100%
/// before it is finished.
pub fn percent(completed: u64, total: u64) -> String {
    if total == 0 {
        return "—".to_owned();
    }
    let pct = (completed as f64 / total as f64 * 100.0).floor();
    format!("{}%", pct.min(100.0) as u64)
}

fn three_sig_figs(v: f64) -> String {
    let s = if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    // Trailing zeros in a live readout twitch as much as a proportional digit does.
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_base_ten_with_three_significant_figures() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1 kB");
        assert_eq!(bytes(1_290_000_000), "1.29 GB");
        assert_eq!(bytes(4_900_000_000), "4.9 GB");
        assert_eq!(bytes(123_400_000), "123 MB");
        assert_eq!(bytes(18_200_000), "18.2 MB");
    }

    #[test]
    fn eta_is_quantized() {
        assert_eq!(eta(41), "41s");
        assert_eq!(eta(260), "4m 20s");
        assert_eq!(eta(300), "5m");
        assert_eq!(eta(4320), "1h 12m");
        assert_eq!(eta(7200), "2h");
    }

    #[test]
    fn percent_never_overstates() {
        assert_eq!(percent(624, 1000), "62%");
        assert_eq!(percent(999, 1000), "99%");
        assert_eq!(percent(1000, 1000), "100%");
        assert_eq!(percent(0, 0), "—");
    }
}
