//! Score / Verdict value types — what one case run produces.
//!
//! The runner (future slice) computes a [`Score`] for each case
//! based on the case's `expected` block and the agent's observed
//! output. The aggregate `Score` then combines with the case's
//! [`crate::ScoringWeights`] to produce a weighted-sum used in the
//! pass/fail decision and in the scorecard's trend lines.

use serde::{Deserialize, Serialize};

/// Per-case score across the four dimensions from
/// `01-agents-and-council.md` §Eval framework.
///
/// All four dimensions are normalized to `[0.0, 1.0]` where 1.0 is
/// "best." Cost is inverted from the raw USD figure
/// (`cost_score = 1.0 / (1.0 + cost_usd)`) so the dimension stays
/// "higher is better" alongside the others. Raw `cost_usd` is kept
/// on the struct for scorecard display.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Score {
    /// 1.0 if the agent produced only the expected actions and no
    /// extras; lower as extras leak in.
    pub precision: f64,
    /// 1.0 if every expected action was produced; lower as the
    /// agent misses expected actions.
    pub recall: f64,
    /// 1.0 if claimed confidence fell within the expected
    /// `[min, max]` band; lower the further out of band.
    pub calibration: f64,
    /// Normalized cost score — `1.0 / (1.0 + cost_usd)`. 1.0 for
    /// free runs, 0.5 for $1, 0.1 for $9, etc.
    pub cost: f64,
    /// Raw cost in USD for the case run. Kept for scorecard
    /// display; `cost` is what factors into the weighted sum.
    pub cost_usd: f64,
}

impl Score {
    /// A perfect score on every dimension (free, in-band, no
    /// extras, no misses). Useful as a baseline in tests and as
    /// the upper bound of the aggregate.
    pub fn perfect() -> Self {
        Self {
            precision: 1.0,
            recall: 1.0,
            calibration: 1.0,
            cost: 1.0,
            cost_usd: 0.0,
        }
    }

    /// Aggregate the four dimensions using the per-case weights.
    /// Returns a single scalar in `[0.0, sum_of_weights]`.
    pub fn weighted_sum(&self, w: &crate::ScoringWeights) -> f64 {
        self.precision * w.precision_weight
            + self.recall * w.recall_weight
            + self.calibration * w.calibration_weight
            + self.cost * w.cost_weight
    }
}

/// Pass / fail / error verdict for one case run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every expected dimension matched.
    Pass,
    /// At least one expected dimension didn't match (precision,
    /// recall, calibration band, missing target, etc.).
    Fail,
    /// The agent threw or panicked, or the runner couldn't even
    /// invoke it. Distinct from `Fail` because errors aren't
    /// scoreable — they get their own row in the scorecard.
    Error,
}

impl Verdict {
    /// Stable string form for persistence into
    /// `eval_case_results.result`. Matches `serde`'s `snake_case`
    /// rename.
    pub fn as_sql(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScoringWeights;

    #[test]
    fn perfect_score_weighted_sum_equals_sum_of_weights() {
        let w = ScoringWeights::default(); // 1 + 1 + 0.5 + 0.2 = 2.7
        assert!((Score::perfect().weighted_sum(&w) - 2.7).abs() < 1e-9);
    }

    #[test]
    fn weighted_sum_drops_to_zero_for_zero_score() {
        let zero = Score {
            precision: 0.0,
            recall: 0.0,
            calibration: 0.0,
            cost: 0.0,
            cost_usd: 0.0,
        };
        assert_eq!(zero.weighted_sum(&ScoringWeights::default()), 0.0);
    }

    #[test]
    fn verdict_sql_is_stable() {
        assert_eq!(Verdict::Pass.as_sql(), "pass");
        assert_eq!(Verdict::Fail.as_sql(), "fail");
        assert_eq!(Verdict::Error.as_sql(), "error");
    }
}
