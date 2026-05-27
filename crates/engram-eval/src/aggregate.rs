//! Aggregate metrics over a set of [`CaseRunResult`]s.
//!
//! The runner (future slice) produces one [`CaseRunResult`] per
//! case; [`Aggregate::from_results`] collapses them into the
//! summary metrics from `01-agents-and-council.md` §Eval framework
//! ("Aggregate metrics" AC bullet).
//!
//! Metrics intentionally treat `Verdict::Error` cases as non-
//! scoreable: they count toward the run total (so pass_rate
//! penalizes errors) but their per-dimension scores are excluded
//! from the means. This matches the spec's "scoreable cases only"
//! convention — a panicking agent should drag pass_rate down
//! without polluting the precision/recall numerator.

use serde::{Deserialize, Serialize};

use crate::score::{Score, Verdict};

/// One case's outcome — what the runner records into
/// `eval_case_results`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseRunResult {
    /// Stable case id from the fixture (e.g. `001-obvious-link`).
    pub case_id: String,
    /// Pass / Fail / Error.
    pub verdict: Verdict,
    /// Per-dimension score. Always present; on `Verdict::Error` the
    /// runner persists a zero-score placeholder so the row schema
    /// stays consistent.
    pub score: Score,
    /// Number of proposals the agent emitted for this case.
    /// Used to normalize cost-per-proposal. `0` is valid for cases
    /// that expected no proposal.
    pub proposals_emitted: usize,
    /// Human-readable reason this case errored or failed. Populated
    /// on `Verdict::Error` (snapshot unpack failure, invoker
    /// panic/timeout, etc.); `None` on Pass and currently on Fail.
    /// Persisted into `eval_case_results.failure_reason`.
    #[serde(default)]
    pub failure_reason: Option<String>,
}

/// Summary metrics over a set of case results.
///
/// `pass_rate` is share of `Verdict::Pass` across **all** cases
/// (errors penalize it). The means are computed over the
/// scoreable subset (`Verdict::Pass` + `Verdict::Fail`) so a
/// single panicking case can't NaN the precision number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aggregate {
    /// Total number of cases in the run (including errors).
    pub total_cases: usize,
    /// Number of cases with `Verdict::Pass`.
    pub passed: usize,
    /// Number of cases with `Verdict::Fail`.
    pub failed: usize,
    /// Number of cases with `Verdict::Error`.
    pub errored: usize,
    /// `passed / total_cases` — drops with both Fail and Error.
    pub pass_rate: f64,
    /// Mean precision over scoreable cases (Pass + Fail).
    pub mean_precision: f64,
    /// Mean recall over scoreable cases.
    pub mean_recall: f64,
    /// Mean calibration error over scoreable cases. Defined as
    /// `1.0 - calibration_score` so 0.0 means perfectly calibrated
    /// (matches the spec's wording, which talks about *error*).
    pub mean_calibration_error: f64,
    /// Mean cost in USD per emitted proposal, averaged over all
    /// scoreable cases that emitted at least one proposal. Zero
    /// when no case emitted proposals.
    pub mean_cost_per_proposal_usd: f64,
}

impl Aggregate {
    /// Empty-aggregate (zero-everything). Useful as a fold seed and
    /// as the answer for an empty result set.
    pub fn empty() -> Self {
        Self {
            total_cases: 0,
            passed: 0,
            failed: 0,
            errored: 0,
            pass_rate: 0.0,
            mean_precision: 0.0,
            mean_recall: 0.0,
            mean_calibration_error: 0.0,
            mean_cost_per_proposal_usd: 0.0,
        }
    }

    /// Compute aggregate over a slice of results. Pure, allocation-
    /// free beyond the returned struct.
    pub fn from_results(results: &[CaseRunResult]) -> Self {
        if results.is_empty() {
            return Self::empty();
        }
        let total_cases = results.len();
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut errored = 0usize;
        let mut sum_precision = 0.0f64;
        let mut sum_recall = 0.0f64;
        let mut sum_calibration_error = 0.0f64;
        let mut sum_cost_per_proposal = 0.0f64;
        let mut scoreable_with_proposals = 0usize;
        let mut scoreable = 0usize;

        for r in results {
            match r.verdict {
                Verdict::Pass => passed += 1,
                Verdict::Fail => failed += 1,
                Verdict::Error => {
                    errored += 1;
                    continue; // exclude error rows from per-dimension means
                }
            }
            scoreable += 1;
            sum_precision += r.score.precision;
            sum_recall += r.score.recall;
            sum_calibration_error += 1.0 - r.score.calibration;
            if r.proposals_emitted > 0 {
                sum_cost_per_proposal += r.score.cost_usd / r.proposals_emitted as f64;
                scoreable_with_proposals += 1;
            }
        }

        let pass_rate = passed as f64 / total_cases as f64;
        let div = scoreable as f64;
        Self {
            total_cases,
            passed,
            failed,
            errored,
            pass_rate,
            mean_precision: if scoreable > 0 {
                sum_precision / div
            } else {
                0.0
            },
            mean_recall: if scoreable > 0 { sum_recall / div } else { 0.0 },
            mean_calibration_error: if scoreable > 0 {
                sum_calibration_error / div
            } else {
                0.0
            },
            mean_cost_per_proposal_usd: if scoreable_with_proposals > 0 {
                sum_cost_per_proposal / scoreable_with_proposals as f64
            } else {
                0.0
            },
        }
    }

    /// Compare this aggregate against `baseline` and return every
    /// metric whose drop exceeds `threshold_pct` (as a fraction —
    /// pass `0.05` for the spec's "5%" gate).
    ///
    /// "Drop" is defined per-metric:
    ///
    /// - **`pass_rate`**, **`mean_precision`**, **`mean_recall`**:
    ///   higher is better. Regression when `current < baseline * (1 - threshold_pct)`.
    /// - **`mean_calibration_error`**, **`mean_cost_per_proposal_usd`**:
    ///   lower is better. Regression when `current > baseline * (1 + threshold_pct)`.
    ///
    /// Baselines where the metric is exactly 0.0 are skipped for the
    /// "lower is better" axes (you can't regress below zero), but a
    /// 0.0 baseline on a "higher is better" axis with a non-zero
    /// current is a *recovery*, never a regression.
    ///
    /// Returns an empty vector when nothing regressed.
    pub fn regressions_against(&self, baseline: &Aggregate, threshold_pct: f64) -> Vec<Regression> {
        let mut out = Vec::new();
        let threshold = threshold_pct.max(0.0);

        // Higher-is-better axes.
        for (name, baseline_v, current_v) in [
            ("pass_rate", baseline.pass_rate, self.pass_rate),
            (
                "mean_precision",
                baseline.mean_precision,
                self.mean_precision,
            ),
            ("mean_recall", baseline.mean_recall, self.mean_recall),
        ] {
            // Drop below `baseline * (1 - threshold)` is a regression.
            // For baseline = 0, any drop is a non-event (the floor).
            if baseline_v > 0.0 && current_v < baseline_v * (1.0 - threshold) {
                out.push(Regression {
                    metric: name,
                    baseline: baseline_v,
                    current: current_v,
                    delta_pct: (current_v - baseline_v) / baseline_v,
                });
            }
        }

        // Lower-is-better axes.
        for (name, baseline_v, current_v) in [
            (
                "mean_calibration_error",
                baseline.mean_calibration_error,
                self.mean_calibration_error,
            ),
            (
                "mean_cost_per_proposal_usd",
                baseline.mean_cost_per_proposal_usd,
                self.mean_cost_per_proposal_usd,
            ),
        ] {
            // Rise above `baseline * (1 + threshold)` is a regression.
            // For baseline = 0 we skip — any positive current is a
            // first-time appearance, not a *regression* per se; the
            // baseline-update step picks it up.
            if baseline_v > 0.0 && current_v > baseline_v * (1.0 + threshold) {
                out.push(Regression {
                    metric: name,
                    baseline: baseline_v,
                    current: current_v,
                    delta_pct: (current_v - baseline_v) / baseline_v,
                });
            }
        }

        out
    }
}

/// One metric's regression vs. a baseline. `delta_pct` is the
/// fractional change `(current - baseline) / baseline`. Negative
/// for higher-is-better axes that dropped; positive for
/// lower-is-better axes that rose.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Regression {
    /// Stable string id of the metric (`"pass_rate"`,
    /// `"mean_precision"`, etc.). Kept as `&'static str` so the
    /// caller can match on it without parsing.
    pub metric: &'static str,
    /// Baseline value the comparison was against.
    pub baseline: f64,
    /// Current value from this aggregate.
    pub current: f64,
    /// Fractional change `(current - baseline) / baseline`.
    /// Negative for higher-is-better drops; positive for
    /// lower-is-better rises.
    pub delta_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing(
        id: &str,
        precision: f64,
        recall: f64,
        cost_usd: f64,
        proposals: usize,
    ) -> CaseRunResult {
        CaseRunResult {
            case_id: id.into(),
            verdict: Verdict::Pass,
            score: Score {
                precision,
                recall,
                calibration: 1.0,
                cost: 1.0 / (1.0 + cost_usd),
                cost_usd,
            },
            proposals_emitted: proposals,
            failure_reason: None,
        }
    }

    fn failing(id: &str, precision: f64, recall: f64, calibration: f64) -> CaseRunResult {
        CaseRunResult {
            case_id: id.into(),
            verdict: Verdict::Fail,
            score: Score {
                precision,
                recall,
                calibration,
                cost: 1.0,
                cost_usd: 0.0,
            },
            proposals_emitted: 1,
            failure_reason: None,
        }
    }

    fn errored(id: &str) -> CaseRunResult {
        CaseRunResult {
            case_id: id.into(),
            verdict: Verdict::Error,
            score: Score {
                precision: 0.0,
                recall: 0.0,
                calibration: 0.0,
                cost: 1.0,
                cost_usd: 0.0,
            },
            proposals_emitted: 0,
            failure_reason: None,
        }
    }

    #[test]
    fn empty_results_yields_empty_aggregate() {
        let agg = Aggregate::from_results(&[]);
        assert_eq!(agg, Aggregate::empty());
    }

    #[test]
    fn all_pass_yields_full_pass_rate() {
        let results = vec![
            passing("a", 1.0, 1.0, 0.0, 1),
            passing("b", 1.0, 1.0, 0.0, 1),
        ];
        let agg = Aggregate::from_results(&results);
        assert_eq!(agg.total_cases, 2);
        assert_eq!(agg.passed, 2);
        assert_eq!(agg.pass_rate, 1.0);
        assert_eq!(agg.mean_precision, 1.0);
        assert_eq!(agg.mean_recall, 1.0);
        assert_eq!(agg.mean_calibration_error, 0.0);
    }

    #[test]
    fn mixed_pass_fail_averages_per_dimension() {
        // Two cases: one perfect, one with precision 0.5 recall 0.5.
        let results = vec![passing("a", 1.0, 1.0, 0.0, 1), failing("b", 0.5, 0.5, 1.0)];
        let agg = Aggregate::from_results(&results);
        assert_eq!(agg.total_cases, 2);
        assert_eq!(agg.passed, 1);
        assert_eq!(agg.failed, 1);
        assert_eq!(agg.pass_rate, 0.5);
        assert_eq!(agg.mean_precision, 0.75);
        assert_eq!(agg.mean_recall, 0.75);
        // Calibration is perfect on both → mean_calibration_error = 0.
        assert_eq!(agg.mean_calibration_error, 0.0);
    }

    #[test]
    fn errors_drag_pass_rate_but_excluded_from_means() {
        // Two cases pass perfectly, one errors. Means computed
        // over the scoreable two only.
        let results = vec![
            passing("a", 1.0, 1.0, 0.0, 1),
            passing("b", 1.0, 1.0, 0.0, 1),
            errored("c"),
        ];
        let agg = Aggregate::from_results(&results);
        assert_eq!(agg.total_cases, 3);
        assert_eq!(agg.passed, 2);
        assert_eq!(agg.errored, 1);
        // pass_rate counts errors against the total:
        assert!((agg.pass_rate - 2.0 / 3.0).abs() < 1e-9);
        // Per-dimension means use scoreable subset (the two passes):
        assert_eq!(agg.mean_precision, 1.0);
        assert_eq!(agg.mean_recall, 1.0);
        assert_eq!(agg.mean_calibration_error, 0.0);
    }

    #[test]
    fn calibration_error_is_one_minus_calibration_score() {
        // calibration = 0.6 → expected calibration_error = 0.4.
        let results = vec![failing("a", 1.0, 1.0, 0.6)];
        let agg = Aggregate::from_results(&results);
        assert!((agg.mean_calibration_error - 0.4).abs() < 1e-9);
    }

    #[test]
    fn cost_per_proposal_averages_only_over_proposing_cases() {
        // Two cases: one with 2 proposals at $0.10 (so $0.05 / prop),
        // one with 0 proposals (excluded from the cost average).
        let results = vec![
            CaseRunResult {
                case_id: "a".into(),
                verdict: Verdict::Pass,
                score: Score {
                    precision: 1.0,
                    recall: 1.0,
                    calibration: 1.0,
                    cost: 1.0,
                    cost_usd: 0.10,
                },
                proposals_emitted: 2,
                failure_reason: None,
            },
            CaseRunResult {
                case_id: "b".into(),
                verdict: Verdict::Pass,
                score: Score::perfect(),
                proposals_emitted: 0,
                failure_reason: None,
            },
        ];
        let agg = Aggregate::from_results(&results);
        assert!((agg.mean_cost_per_proposal_usd - 0.05).abs() < 1e-9);
    }

    #[test]
    fn cost_per_proposal_zero_when_no_case_emitted_proposals() {
        let results = vec![CaseRunResult {
            case_id: "a".into(),
            verdict: Verdict::Pass,
            score: Score::perfect(),
            proposals_emitted: 0,
            failure_reason: None,
        }];
        let agg = Aggregate::from_results(&results);
        assert_eq!(agg.mean_cost_per_proposal_usd, 0.0);
    }

    #[test]
    fn all_errors_yields_zero_pass_rate_and_zero_means() {
        let results = vec![errored("a"), errored("b")];
        let agg = Aggregate::from_results(&results);
        assert_eq!(agg.pass_rate, 0.0);
        assert_eq!(agg.mean_precision, 0.0);
        assert_eq!(agg.mean_recall, 0.0);
        assert_eq!(agg.mean_calibration_error, 0.0);
        assert_eq!(agg.errored, 2);
    }

    // ─── regressions_against ──────────────────────────────────────

    fn agg(precision: f64, recall: f64, cal_err: f64, cost: f64) -> Aggregate {
        Aggregate {
            total_cases: 10,
            passed: 8,
            failed: 2,
            errored: 0,
            pass_rate: 0.8,
            mean_precision: precision,
            mean_recall: recall,
            mean_calibration_error: cal_err,
            mean_cost_per_proposal_usd: cost,
        }
    }

    #[test]
    fn identical_aggregates_have_no_regressions() {
        let a = agg(0.9, 0.9, 0.1, 0.05);
        assert!(a.regressions_against(&a, 0.05).is_empty());
    }

    #[test]
    fn precision_drop_just_over_threshold_is_regression() {
        let baseline = agg(1.0, 1.0, 0.0, 0.0);
        // 5.1% drop on precision crosses the 5% threshold.
        let current = agg(0.949, 1.0, 0.0, 0.0);
        let regs = current.regressions_against(&baseline, 0.05);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].metric, "mean_precision");
        assert!((regs[0].current - 0.949).abs() < 1e-9);
        assert!(regs[0].delta_pct < -0.05);
    }

    #[test]
    fn precision_drop_just_under_threshold_is_not_regression() {
        let baseline = agg(1.0, 1.0, 0.0, 0.0);
        // 4% drop on precision is within tolerance.
        let current = agg(0.96, 1.0, 0.0, 0.0);
        assert!(current.regressions_against(&baseline, 0.05).is_empty());
    }

    #[test]
    fn calibration_error_rise_over_threshold_is_regression() {
        // calibration_error baseline 0.10, new 0.11 → +10%, exceeds 5%.
        let baseline = agg(1.0, 1.0, 0.10, 0.0);
        let current = agg(1.0, 1.0, 0.11, 0.0);
        let regs = current.regressions_against(&baseline, 0.05);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].metric, "mean_calibration_error");
        assert!(regs[0].delta_pct > 0.05);
    }

    #[test]
    fn cost_rise_over_threshold_is_regression() {
        let baseline = agg(1.0, 1.0, 0.0, 0.10);
        let current = agg(1.0, 1.0, 0.0, 0.20);
        let regs = current.regressions_against(&baseline, 0.05);
        assert!(regs
            .iter()
            .any(|r| r.metric == "mean_cost_per_proposal_usd"));
    }

    #[test]
    fn baseline_zero_on_lower_is_better_axis_is_skipped() {
        // Baseline calibration_error = 0 → we can't compute a
        // meaningful relative regression; the check is a no-op.
        let baseline = agg(1.0, 1.0, 0.0, 0.0);
        let current = agg(1.0, 1.0, 0.5, 0.0);
        assert!(current.regressions_against(&baseline, 0.05).is_empty());
    }

    #[test]
    fn baseline_zero_on_higher_is_better_axis_is_skipped() {
        // Baseline pass_rate = 0 → can't compute relative
        // regression; any current value is a recovery.
        let baseline = Aggregate::empty();
        let current = agg(1.0, 1.0, 0.0, 0.0);
        assert!(current.regressions_against(&baseline, 0.05).is_empty());
    }

    #[test]
    fn multiple_regressions_all_surface() {
        let baseline = agg(1.0, 1.0, 0.05, 0.05);
        // Precision -10%, recall -8%, cal_err +30%, cost +50%.
        let current = agg(0.90, 0.92, 0.065, 0.075);
        let regs = current.regressions_against(&baseline, 0.05);
        let metrics: Vec<&str> = regs.iter().map(|r| r.metric).collect();
        assert!(metrics.contains(&"mean_precision"));
        assert!(metrics.contains(&"mean_recall"));
        assert!(metrics.contains(&"mean_calibration_error"));
        assert!(metrics.contains(&"mean_cost_per_proposal_usd"));
    }

    #[test]
    fn improvements_never_count_as_regressions() {
        let baseline = agg(0.8, 0.8, 0.20, 0.10);
        // All metrics improve.
        let current = agg(0.95, 0.95, 0.05, 0.02);
        assert!(current.regressions_against(&baseline, 0.05).is_empty());
    }
}
