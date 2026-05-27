//! Markdown scorecard emitter.
//!
//! Renders a current [`Aggregate`] plus up-to-7 prior aggregates
//! into the scorecard markdown described in
//! `01-agents-and-council.md` §Eval framework ("Scorecard
//! regeneration: ... 8-run trend sparklines per metric").
//!
//! Pure function. The runner (future slice) writes the result to
//! `.engram/evals/<agent>/scorecard.md`; this module just formats.
//!
//! # Sparkline format
//!
//! Unicode block characters at 8 levels: ` ` (empty), ▁ ▂ ▃ ▄ ▅ ▆ ▇ █.
//! Each metric's sparkline shows up to 8 points: the current run
//! plus the 7 most recent prior runs, oldest to newest. The
//! mapping is per-metric range — a metric whose history spans
//! 0.5..1.0 uses the full 8-level scale across that band so trends
//! stay legible regardless of absolute values.

use std::fmt::Write;

use crate::aggregate::Aggregate;

/// Render a scorecard markdown report for one agent.
///
/// `current` is the most recent run. `history` is the up-to-7
/// prior runs, oldest first. If `history` is empty, sparklines
/// show a single point for the current value; if `history` is
/// longer than 7 entries, only the last 7 are shown.
///
/// Returns a markdown string ready to write to
/// `.engram/evals/<agent>/scorecard.md`.
pub fn render_scorecard(agent: &str, current: &Aggregate, history: &[Aggregate]) -> String {
    let trimmed = trim_history(history, 7);
    let trail: Vec<&Aggregate> = trimmed.iter().chain(std::iter::once(current)).collect();

    let mut out = String::new();
    let _ = writeln!(out, "# {agent} eval scorecard");
    let _ = writeln!(out);
    let _ = writeln!(out, "## Current run");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- Total cases: **{}** (passed {}, failed {}, errored {})",
        current.total_cases, current.passed, current.failed, current.errored
    );
    let _ = writeln!(out, "- Pass rate: **{:.1}%**", current.pass_rate * 100.0);
    let _ = writeln!(out, "- Mean precision: **{:.3}**", current.mean_precision);
    let _ = writeln!(out, "- Mean recall: **{:.3}**", current.mean_recall);
    let _ = writeln!(
        out,
        "- Mean calibration error: **{:.3}**",
        current.mean_calibration_error
    );
    let _ = writeln!(
        out,
        "- Mean cost/proposal: **${:.4}**",
        current.mean_cost_per_proposal_usd
    );
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "## Trend (oldest → newest, last {} run{})",
        trail.len(),
        if trail.len() == 1 { "" } else { "s" }
    );
    let _ = writeln!(out);

    let pass_rates: Vec<f64> = trail.iter().map(|a| a.pass_rate).collect();
    let precisions: Vec<f64> = trail.iter().map(|a| a.mean_precision).collect();
    let recalls: Vec<f64> = trail.iter().map(|a| a.mean_recall).collect();
    let cal_errors: Vec<f64> = trail.iter().map(|a| a.mean_calibration_error).collect();
    let costs: Vec<f64> = trail.iter().map(|a| a.mean_cost_per_proposal_usd).collect();

    // Pass rate, precision, recall: higher is better.
    let _ = writeln!(out, "| Metric | Trend | Latest |");
    let _ = writeln!(out, "|--------|-------|--------|");
    let _ = writeln!(
        out,
        "| Pass rate | `{}` | {:.1}% |",
        sparkline(&pass_rates),
        current.pass_rate * 100.0
    );
    let _ = writeln!(
        out,
        "| Mean precision | `{}` | {:.3} |",
        sparkline(&precisions),
        current.mean_precision
    );
    let _ = writeln!(
        out,
        "| Mean recall | `{}` | {:.3} |",
        sparkline(&recalls),
        current.mean_recall
    );
    // Calibration error: lower is better, but sparkline mapping is
    // value-relative so trend direction is visible regardless.
    let _ = writeln!(
        out,
        "| Mean calibration error | `{}` | {:.3} |",
        sparkline(&cal_errors),
        current.mean_calibration_error
    );
    let _ = writeln!(
        out,
        "| Mean cost/proposal (USD) | `{}` | ${:.4} |",
        sparkline(&costs),
        current.mean_cost_per_proposal_usd
    );

    out
}

/// Take the last `n` history entries (or all if shorter).
fn trim_history(history: &[Aggregate], n: usize) -> &[Aggregate] {
    if history.len() <= n {
        history
    } else {
        &history[history.len() - n..]
    }
}

/// Render a series of values as a Unicode sparkline. Each value
/// maps to one of 8 levels (▁ through █) scaled to the series's
/// own min/max so trend direction is visible regardless of
/// absolute scale. A series of identical values renders as all
/// `▄` (mid-level) — there's no trend to show.
pub fn sparkline(values: &[f64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    let mut out = String::with_capacity(values.len() * 3);
    for &v in values {
        let level = if range.abs() < f64::EPSILON {
            // Flat series — render mid-level so the reader sees
            // "no movement" rather than misleading bars.
            3
        } else {
            let normalized = (v - min) / range; // 0.0 ..= 1.0
            let scaled = (normalized * (LEVELS.len() as f64 - 1.0)).round() as usize;
            scaled.min(LEVELS.len() - 1)
        };
        out.push(LEVELS[level]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(pass_rate: f64, precision: f64, recall: f64, cal_err: f64, cost: f64) -> Aggregate {
        Aggregate {
            total_cases: 10,
            passed: (pass_rate * 10.0).round() as usize,
            failed: 10 - (pass_rate * 10.0).round() as usize,
            errored: 0,
            pass_rate,
            mean_precision: precision,
            mean_recall: recall,
            mean_calibration_error: cal_err,
            mean_cost_per_proposal_usd: cost,
        }
    }

    #[test]
    fn sparkline_empty_series_renders_empty_string() {
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn sparkline_single_value_renders_one_character() {
        let s = sparkline(&[0.5]);
        assert_eq!(s.chars().count(), 1);
    }

    #[test]
    fn sparkline_flat_series_renders_mid_level() {
        // All identical → no trend → ▄▄▄ at mid level (index 3).
        let s = sparkline(&[0.5, 0.5, 0.5]);
        assert_eq!(s, "▄▄▄");
    }

    #[test]
    fn sparkline_monotonic_increase_renders_low_to_high() {
        let s = sparkline(&[0.0, 0.25, 0.5, 0.75, 1.0]);
        // Min=0, max=1, 5 levels of 8 evenly spaced.
        // 0.0 → 0, 0.25 → 2, 0.5 → 4, 0.75 → 5/6, 1.0 → 7.
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars[0], '▁');
        assert_eq!(chars[chars.len() - 1], '█');
        // Monotonic increase: each level >= previous.
        let levels: Vec<usize> = chars
            .iter()
            .map(|c| match c {
                '▁' => 0,
                '▂' => 1,
                '▃' => 2,
                '▄' => 3,
                '▅' => 4,
                '▆' => 5,
                '▇' => 6,
                '█' => 7,
                _ => panic!("non-sparkline char"),
            })
            .collect();
        for w in levels.windows(2) {
            assert!(w[1] >= w[0], "expected monotone non-decreasing");
        }
    }

    #[test]
    fn render_scorecard_contains_agent_name_and_metrics() {
        let current = agg(0.8, 0.9, 0.85, 0.1, 0.012);
        let history = vec![agg(0.7, 0.8, 0.75, 0.2, 0.020)];
        let md = render_scorecard("linker", &current, &history);
        assert!(md.contains("# linker eval scorecard"));
        assert!(md.contains("Pass rate: **80.0%**"));
        assert!(md.contains("Mean precision: **0.900**"));
        assert!(md.contains("Mean recall: **0.850**"));
        assert!(md.contains("Mean calibration error: **0.100**"));
        assert!(md.contains("Mean cost/proposal: **$0.0120**"));
        // The trend table must include a row per metric.
        assert!(md.contains("| Pass rate | "));
        assert!(md.contains("| Mean precision | "));
        assert!(md.contains("| Mean recall | "));
        assert!(md.contains("| Mean calibration error | "));
        assert!(md.contains("| Mean cost/proposal (USD) | "));
    }

    #[test]
    fn render_scorecard_with_no_history_still_includes_current_in_trend() {
        let current = agg(0.5, 0.5, 0.5, 0.5, 0.5);
        let md = render_scorecard("gardener", &current, &[]);
        assert!(md.contains("oldest → newest, last 1 run"));
        // With one data point, sparklines are one mid-level char.
        assert!(md.contains("▄"));
    }

    #[test]
    fn render_scorecard_trims_history_to_seven_priors() {
        let priors: Vec<Aggregate> = (0..15)
            .map(|i| agg(i as f64 / 14.0, 0.5, 0.5, 0.5, 0.5))
            .collect();
        let current = agg(1.0, 1.0, 1.0, 0.0, 0.0);
        let md = render_scorecard("scribe", &current, &priors);
        // 7 priors + 1 current = 8 data points in each sparkline.
        assert!(md.contains("last 8 runs"));
    }
}
