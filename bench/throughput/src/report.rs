//! What the harness prints.
//!
//! Two audiences. The console table is for whoever just changed the scheduler and wants to
//! know within one screen whether they broke the no-regression floor. The markdown table is
//! the artifact: it replaces the performance stance in the README with measured numbers,
//! and it carries the conditions with it, because a throughput figure without its fixture
//! is a rumour.

use std::fmt::Write as _;
use std::time::Duration;

use crate::stats::Summary;

pub struct Row {
    pub id: &'static str,
    /// What was set up, in the words the README will use. What each fixture is *for* lives
    /// in `bench/README.md`.
    pub conditions: String,
    pub result: Outcome,
}

pub enum Outcome {
    Measured(Measured),
    /// Not runnable here — printed rather than dropped. A harness that silently omits a
    /// fixture reads as though it measured everything.
    NotRun(String),
}

pub struct Measured {
    pub trials: usize,
    /// What actually came down the wire, which is the only way a remote fixture knows how
    /// big its object was.
    pub bytes: u64,
    /// Seconds, both sides, and the two paired comparisons between them. Paired, because a
    /// machine that drifts mid-run drifts for both sides and the ratio should not notice.
    pub vortex: Summary,
    pub baseline: Summary,
    pub speedup: Summary,
    /// Seconds over the baseline. Negative means Vortex finished first.
    pub overhead: Summary,
    pub growth: Option<u64>,
    /// The most connections the engine opened at once, across every trial.
    pub lanes: u8,
    pub floor: Option<f64>,
    pub ceiling: Option<u64>,
    pub budget: Option<Duration>,
    pub allowed_lanes: Option<u8>,
    /// Empty when the fixture met every floor it was given.
    pub failures: Vec<String>,
}

impl Measured {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

pub fn console(rows: &[Row]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<14} {:>3}  {:>9} {:>9}  {:<20} {:>10} {:>5} {:>9}  verdict",
        "fixture", "n", "vortex", "curl", "speedup", "overhead", "conn", "memory"
    );
    let _ = writeln!(out, "{}", "-".repeat(110));
    for row in rows {
        match &row.result {
            Outcome::NotRun(why) => {
                let _ = writeln!(
                    out,
                    "{:<14} {:>3}  {:>9} {:>9}  {:<20} {:>10} {:>5} {:>9}  not run: {}",
                    row.id,
                    "-",
                    "-",
                    "-",
                    "-",
                    "-",
                    "-",
                    "-",
                    first_sentence(why)
                );
            }
            Outcome::Measured(m) => {
                let _ = writeln!(
                    out,
                    "{:<14} {:>3}  {:>9} {:>9}  {:<20} {:>10} {:>5} {:>9}  {}",
                    row.id,
                    m.trials,
                    secs(m.vortex.median),
                    secs(m.baseline.median),
                    format!(
                        "{:.2}x ({:.2}-{:.2})",
                        m.speedup.median, m.speedup.p10, m.speedup.p90
                    ),
                    format!("{:+.0} ms", m.overhead.median * 1000.0),
                    m.lanes,
                    m.growth.map(growth).unwrap_or_else(|| "-".into()),
                    if m.passed() {
                        "pass".to_owned()
                    } else {
                        format!("FAIL  {}", m.failures.join("; "))
                    }
                );
            }
        }
    }
    out
}

pub fn markdown(rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str("| Fixture | Conditions | vs `curl` | Memory | Held to |\n");
    out.push_str("|---|---|---|---|---|\n");
    for row in rows {
        let (measured, memory) = match &row.result {
            Outcome::NotRun(why) => (format!("not run — {why}"), "—".to_owned()),
            Outcome::Measured(m) => (
                format!(
                    "**{:.2}×** (p10 {:.2}, p90 {:.2}, n={}), {:+.0} ms",
                    m.speedup.median,
                    m.speedup.p10,
                    m.speedup.p90,
                    m.trials,
                    m.overhead.median * 1000.0
                ),
                m.growth.map(growth).unwrap_or_else(|| "—".into()),
            ),
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} |",
            row.id,
            row.conditions,
            measured,
            memory,
            held_to(&row.result)
        );
    }
    out.push_str(
        "\nEach fixture runs a discarded warm-up pair and then `n` alternating pairs; both \
         the ratio and the overhead are paired per trial, so a machine that drifts mid-run \
         drifts for both sides. Every trial's output is hashed, and a local fixture's hash \
         is checked against what the origin knows it served.\n",
    );
    out
}

/// The constraints the fixture was actually gated on, with whether it met them. A fixture
/// can carry more than one; a fixture with none is a measurement, not a gate.
fn held_to(result: &Outcome) -> String {
    let Outcome::Measured(m) = result else {
        return "—".to_owned();
    };
    let tick = if m.passed() { "✓" } else { "✗" };
    let mut held = Vec::new();
    if let Some(floor) = m.floor {
        held.push(format!("≥ {floor:.2}×"));
    }
    if let Some(budget) = m.budget {
        held.push(format!("≤ {:.0} ms overhead", budget.as_secs_f64() * 1000.0));
    }
    if let Some(ceiling) = m.ceiling {
        held.push(format!("< {} resident", vortex_proto::fmt::bytes(ceiling)));
    }
    if let Some(lanes) = m.allowed_lanes {
        held.push(format!("≤ {lanes} connection(s)"));
    }
    if held.is_empty() {
        return "—".to_owned();
    }
    format!("{} {tick}", held.join(", "))
}

/// The console has one line per fixture; the full reason lives in the markdown.
fn first_sentence(why: &str) -> String {
    match why.find(". ") {
        Some(end) => format!("{}…", &why[..=end]),
        None => why.to_owned(),
    }
}

/// Three significant figures, like every other number this product prints.
fn secs(v: f64) -> String {
    if v >= 100.0 {
        format!("{v:.0} s")
    } else if v >= 10.0 {
        format!("{v:.1} s")
    } else if v >= 1.0 {
        format!("{v:.2} s")
    } else {
        format!("{:.0} ms", v * 1000.0)
    }
}

fn growth(bytes: u64) -> String {
    format!("+{}", vortex_proto::fmt::bytes(bytes))
}
