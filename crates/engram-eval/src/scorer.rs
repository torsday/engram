//! Pure scoring function: given a case's
//! [`ExpectedOutcome`](crate::ExpectedOutcome) and an
//! [`Observation`] of what the agent actually did, produce a
//! [`Score`] + [`Verdict`].
//!
//! No I/O. The runner (future slice) is responsible for invoking
//! the agent, unpacking the vault snapshot, and turning the
//! agent's `RunReport` into an [`Observation`]; this module just
//! computes the verdict.
//!
//! # Dimension semantics
//!
//! - **Precision**: of all proposed link targets, what fraction
//!   matches `expected.target_id`? When no target is expected,
//!   precision is 1.0 if no links are proposed and 0.0 otherwise.
//! - **Recall**: did the expected target appear among the
//!   proposals? Binary 1.0 / 0.0 when `expected.target_id` is set;
//!   1.0 when no target is expected.
//! - **Calibration**: agent's claimed `confidence` falls within
//!   `[min_confidence, max_confidence]`. Inside the band → 1.0;
//!   outside → linear falloff to 0.0 at distance 0.5. Absent
//!   confidence is treated as 0.5 (neutral).
//! - **Cost**: normalized as `1.0 / (1.0 + cost_usd)` — perfect
//!   for free runs, decays smoothly for expensive ones.
//!
//! # Verdict rule
//!
//! `Pass` when every *applicable* expected dimension matches
//! strictly (precision ≥ 1.0, recall ≥ 1.0, confidence inside band,
//! every required rationale keyword present). `Fail` otherwise.
//! `Error` is reserved for the runner's catch-fall when invocation
//! threw — the scorer never emits it from observations alone.

use crate::case::ExpectedOutcome;
use crate::score::{Score, Verdict};

/// What the runner observed when invoking the agent against the
/// case's seeded vault. Mirrors the subset of `RunReport` the
/// scorer cares about — keeping it a separate value type means the
/// runner can be ported across `RunReport` evolutions without
/// rewriting the scorer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observation {
    /// Link targets the agent proposed (`target_id` per change).
    /// Empty when the agent proposed nothing.
    pub proposed_link_targets: Vec<String>,
    /// Confidence the agent self-reported (0.0–1.0), if any.
    pub confidence: Option<f64>,
    /// Rationale string the agent emitted, if any. Compared
    /// case-insensitively against `expected.rationale_must_mention`.
    pub rationale: Option<String>,
    /// USD cost of the case run. Used in the cost dimension and
    /// stored verbatim on the [`Score`] for scorecard display.
    pub cost_usd: f64,
}

/// Score `observed` against `expected`. Returns `(Score, Verdict)`.
/// Pure function — no I/O, no allocation beyond returned struct.
pub fn score_case(expected: &ExpectedOutcome, observed: &Observation) -> (Score, Verdict) {
    let precision = compute_precision(expected, observed);
    let recall = compute_recall(expected, observed);
    let calibration = compute_calibration(expected, observed);
    let cost = 1.0 / (1.0 + observed.cost_usd.max(0.0));

    let score = Score {
        precision,
        recall,
        calibration,
        cost,
        cost_usd: observed.cost_usd,
    };

    // Pass requires every *applicable* dimension to land at its
    // strict best (1.0). Cost is excluded — there's no spec
    // ceiling, so a high-cost case can still pass on correctness.
    let proposes_link_matches = match expected.proposes_link {
        Some(true) => !observed.proposed_link_targets.is_empty(),
        Some(false) => observed.proposed_link_targets.is_empty(),
        None => true,
    };
    let rationale_keywords_match = if expected.rationale_must_mention.is_empty() {
        true
    } else {
        let hay = observed
            .rationale
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        expected
            .rationale_must_mention
            .iter()
            .all(|kw| hay.contains(&kw.to_ascii_lowercase()))
    };
    let verdict = if proposes_link_matches
        && precision >= 1.0
        && recall >= 1.0
        && calibration >= 1.0
        && rationale_keywords_match
    {
        Verdict::Pass
    } else {
        Verdict::Fail
    };

    (score, verdict)
}

fn compute_precision(expected: &ExpectedOutcome, observed: &Observation) -> f64 {
    match (expected.target_id.as_deref(), expected.proposes_link) {
        // Expected no link AND none proposed → perfect precision.
        // Expected no link AND links proposed → zero precision.
        (_, Some(false)) => {
            if observed.proposed_link_targets.is_empty() {
                1.0
            } else {
                0.0
            }
        }
        // No specific target expected — measure "did the agent
        // propose at most one link?" as the precision proxy.
        (None, _) => {
            if observed.proposed_link_targets.is_empty() {
                1.0
            } else {
                1.0 / observed.proposed_link_targets.len() as f64
            }
        }
        // Specific target expected — precision is (correct / total).
        (Some(target), _) => {
            if observed.proposed_link_targets.is_empty() {
                // No proposals; nothing wrong, but nothing right either.
                // Treat as 1.0 — recall will drop and verdict fail.
                1.0
            } else {
                let hits = observed
                    .proposed_link_targets
                    .iter()
                    .filter(|t| t.as_str() == target)
                    .count();
                hits as f64 / observed.proposed_link_targets.len() as f64
            }
        }
    }
}

fn compute_recall(expected: &ExpectedOutcome, observed: &Observation) -> f64 {
    match (expected.target_id.as_deref(), expected.proposes_link) {
        // No target expected → no recall test.
        (None, _) => 1.0,
        (Some(target), _) => {
            if observed
                .proposed_link_targets
                .iter()
                .any(|t| t.as_str() == target)
            {
                1.0
            } else {
                0.0
            }
        }
    }
}

fn compute_calibration(expected: &ExpectedOutcome, observed: &Observation) -> f64 {
    let (min, max) = (expected.min_confidence, expected.max_confidence);
    if min.is_none() && max.is_none() {
        return 1.0; // no band → no test
    }
    // Treat absent confidence as neutral 0.5 — the agent didn't
    // self-report, so we can't reward or punish calibration.
    let c = observed.confidence.unwrap_or(0.5);
    let lo = min.unwrap_or(0.0);
    let hi = max.unwrap_or(1.0);
    if c >= lo && c <= hi {
        1.0
    } else {
        // Linear falloff: distance 0 → 1.0, distance ≥ 0.5 → 0.0.
        let dist = if c < lo { lo - c } else { c - hi };
        (1.0 - dist / 0.5).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect(target: Option<&str>, proposes: Option<bool>) -> ExpectedOutcome {
        ExpectedOutcome {
            proposes_link: proposes,
            target_id: target.map(String::from),
            min_confidence: None,
            max_confidence: None,
            rationale_must_mention: Vec::new(),
        }
    }

    #[test]
    fn matching_proposal_passes() {
        let exp = expect(Some("01HX"), Some(true));
        let obs = Observation {
            proposed_link_targets: vec!["01HX".into()],
            confidence: Some(0.9),
            rationale: Some("strong semantic match".into()),
            cost_usd: 0.0,
        };
        let (score, verdict) = score_case(&exp, &obs);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(score.precision, 1.0);
        assert_eq!(score.recall, 1.0);
    }

    #[test]
    fn missing_target_fails_with_zero_recall() {
        let exp = expect(Some("01HX"), Some(true));
        let obs = Observation {
            proposed_link_targets: vec!["01OTHER".into()],
            ..Default::default()
        };
        let (score, verdict) = score_case(&exp, &obs);
        assert_eq!(verdict, Verdict::Fail);
        assert_eq!(score.recall, 0.0);
    }

    #[test]
    fn extra_proposals_drop_precision() {
        let exp = expect(Some("01HX"), Some(true));
        let obs = Observation {
            proposed_link_targets: vec!["01HX".into(), "01OTHER".into(), "01ANOTHER".into()],
            ..Default::default()
        };
        let (score, verdict) = score_case(&exp, &obs);
        // 1 correct of 3 → precision = 1/3.
        assert!((score.precision - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(verdict, Verdict::Fail);
    }

    #[test]
    fn proposes_link_false_with_no_proposals_passes() {
        let exp = expect(None, Some(false));
        let obs = Observation::default();
        let (score, verdict) = score_case(&exp, &obs);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(score.precision, 1.0);
        assert_eq!(score.recall, 1.0);
    }

    #[test]
    fn proposes_link_false_with_proposals_fails_at_zero_precision() {
        let exp = expect(None, Some(false));
        let obs = Observation {
            proposed_link_targets: vec!["01ANY".into()],
            ..Default::default()
        };
        let (score, verdict) = score_case(&exp, &obs);
        assert_eq!(score.precision, 0.0);
        assert_eq!(verdict, Verdict::Fail);
    }

    #[test]
    fn calibration_inside_band_is_perfect() {
        let exp = ExpectedOutcome {
            min_confidence: Some(0.7),
            max_confidence: Some(0.95),
            ..Default::default()
        };
        let obs = Observation {
            confidence: Some(0.85),
            ..Default::default()
        };
        let (score, _) = score_case(&exp, &obs);
        assert_eq!(score.calibration, 1.0);
    }

    #[test]
    fn calibration_just_outside_band_drops_below_one() {
        let exp = ExpectedOutcome {
            min_confidence: Some(0.7),
            ..Default::default()
        };
        let obs = Observation {
            confidence: Some(0.5), // 0.2 below the floor
            ..Default::default()
        };
        let (score, verdict) = score_case(&exp, &obs);
        // 1 - 0.2 / 0.5 = 0.6
        assert!((score.calibration - 0.6).abs() < 1e-9);
        assert_eq!(verdict, Verdict::Fail);
    }

    #[test]
    fn calibration_far_outside_band_clamps_to_zero() {
        let exp = ExpectedOutcome {
            min_confidence: Some(0.7),
            ..Default::default()
        };
        let obs = Observation {
            confidence: Some(0.0),
            ..Default::default()
        };
        let (score, _) = score_case(&exp, &obs);
        assert_eq!(score.calibration, 0.0);
    }

    #[test]
    fn rationale_keyword_check_is_case_insensitive() {
        let exp = ExpectedOutcome {
            target_id: Some("01HX".into()),
            proposes_link: Some(true),
            rationale_must_mention: vec!["Semantic".into(), "AGREEMENT".into()],
            ..Default::default()
        };
        let obs = Observation {
            proposed_link_targets: vec!["01HX".into()],
            rationale: Some("strong semantic overlap and agreement".into()),
            ..Default::default()
        };
        let (_, verdict) = score_case(&exp, &obs);
        assert_eq!(verdict, Verdict::Pass);
    }

    #[test]
    fn missing_required_keyword_fails() {
        let exp = ExpectedOutcome {
            target_id: Some("01HX".into()),
            proposes_link: Some(true),
            rationale_must_mention: vec!["semantic".into()],
            ..Default::default()
        };
        let obs = Observation {
            proposed_link_targets: vec!["01HX".into()],
            rationale: Some("nothing relevant".into()),
            ..Default::default()
        };
        let (_, verdict) = score_case(&exp, &obs);
        assert_eq!(verdict, Verdict::Fail);
    }

    #[test]
    fn cost_dimension_decays_with_spend() {
        let exp = ExpectedOutcome::default();
        let cheap = Observation {
            cost_usd: 0.0,
            ..Default::default()
        };
        let expensive = Observation {
            cost_usd: 1.0,
            ..Default::default()
        };
        let (cheap_score, _) = score_case(&exp, &cheap);
        let (exp_score, _) = score_case(&exp, &expensive);
        assert_eq!(cheap_score.cost, 1.0);
        assert!((exp_score.cost - 0.5).abs() < 1e-9);
    }

    #[test]
    fn empty_expected_against_empty_observation_passes() {
        let exp = ExpectedOutcome::default();
        let obs = Observation::default();
        let (score, verdict) = score_case(&exp, &obs);
        assert_eq!(score, Score::perfect());
        assert_eq!(verdict, Verdict::Pass);
    }
}
