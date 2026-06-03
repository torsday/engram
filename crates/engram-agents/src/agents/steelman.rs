//! Typed output schema for the Steelman (gate role) agent.
//!
//! Mirrors the JSON schema documented in `agents/steelman/prompt.md`
//! § "Output schema". The gate role is the mandatory rationality
//! check on every critical agent's critique (ADR 0007): it judges a
//! critique against five criteria and reports a verdict.
//!
//! This struct describes what the LLM *emits*. The authoritative
//! verdict — and the one-revision rule — is recomputed from the
//! per-criterion booleans by the pure decision core in
//! `engram_council::gate` (`SteelmanGate::evaluate`); the LLM's own
//! `verdict` field is its self-report, and a mismatch between it and
//! the `criteria` booleans is a transcript-recorded signal.
//!
//! Per ADR 0011, `confidence` and `rationale` stream first so the
//! runner can early-exit before the `criteria` payload. The on-disk
//! prompt schema and this struct must change together in one PR.

use serde::{Deserialize, Serialize};

/// Top-level output from the Steelman gate-role agent.
///
/// Field order is the ADR 0011 streaming contract: `confidence`,
/// then `rationale`, then the cheap `verdict` discriminant, then the
/// `criteria` payload. See `agents/steelman/prompt.md` for the prompt
/// that produces this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteelmanOutput {
    /// Self-assessed confidence (0.0–1.0) in this gate judgment.
    /// Streams first per ADR 0011.
    pub confidence: f32,

    /// One paragraph: why this verdict, and what could be wrong with
    /// the judgment. Streams second.
    pub rationale: String,

    /// The agent's self-reported verdict. The council recomputes the
    /// authoritative verdict from [`criteria`](Self::criteria) (all
    /// five `held` → pass) and applies the one-revision rule, so this
    /// field is advisory — but a mismatch with `criteria` is logged.
    pub verdict: SteelmanVerdict,

    /// Per-criterion judgment against the five ADR 0007 criteria. The
    /// council maps these booleans to `engram_council::gate::FiveCriteria`.
    pub criteria: CriteriaAssessment,
}

/// The gate's verdict on a critique.
///
/// A typed enum, never a free-form string (ADR 0016 §5): a
/// hallucinated verdict name fails to parse rather than silently
/// routing to a missing branch. Serialized `kebab-case` to match the
/// prompt's documented values (`pass`, `request-revision`, `shelve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SteelmanVerdict {
    /// All five criteria held — the critique counts.
    Pass,
    /// One or more criteria failed on the first pass — one revision
    /// attempt allowed.
    RequestRevision,
    /// The critique failed after its one revision — shelved as "no
    /// defensible critique found".
    Shelve,
}

/// The five-criterion judgment block.
///
/// One [`CriterionJudgment`] per ADR 0007 criterion. The field names
/// match `engram_council::gate::FiveCriteria` so the wiring layer
/// (#317) maps them mechanically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriteriaAssessment {
    /// Criterion 1 — engages the actual claim, not a strawman.
    pub engages_actual_claim: CriterionJudgment,
    /// Criterion 2 — cites real evidence, not bare assertion.
    pub uses_real_evidence: CriterionJudgment,
    /// Criterion 3 — a coherent alternative, not mere negation.
    pub internally_consistent: CriterionJudgment,
    /// Criterion 4 — a respected thinker could plausibly hold it.
    pub has_real_world_adherents: CriterionJudgment,
    /// Criterion 5 — concedes what the original got right.
    pub concedes_whats_true: CriterionJudgment,
}

impl CriteriaAssessment {
    /// `true` iff all five criteria held — the only assessment that
    /// passes the gate. Mirrors
    /// `engram_council::gate::FiveCriteria::all_pass`.
    pub fn all_held(&self) -> bool {
        self.engages_actual_claim.held
            && self.uses_real_evidence.held
            && self.internally_consistent.held
            && self.has_real_world_adherents.held
            && self.concedes_whats_true.held
    }
}

/// One criterion's judgment: whether it held, and a one-sentence why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionJudgment {
    /// Whether this criterion holds for the critique under review.
    pub held: bool,
    /// One sentence justifying the `held` verdict — recorded in the
    /// deliberation transcript so the critic (and the human) can see
    /// *why* a criterion passed or failed.
    pub why: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative `pass` output: all five criteria held.
    const PASS_OUTPUT: &str = r#"{
        "confidence": 0.82,
        "rationale": "The critique engages the note's actual load-bearing claim, cites two neighbor notes as counter-evidence, and concedes the original's framing is useful before challenging its scope.",
        "verdict": "pass",
        "criteria": {
            "engages_actual_claim": {"held": true, "why": "Restates the note's central claim accurately before arguing."},
            "uses_real_evidence": {"held": true, "why": "Cites 01H8QZ and 01H8RC from the neighbor set."},
            "internally_consistent": {"held": true, "why": "The counter-position holds together as a coherent alternative."},
            "has_real_world_adherents": {"held": true, "why": "A reliabilist epistemologist would hold this view."},
            "concedes_whats_true": {"held": true, "why": "Grants the original's core distinction before narrowing it."}
        }
    }"#;

    /// A `shelve` output: a strawman critique that failed criterion 1.
    const SHELVE_OUTPUT: &str = r#"{
        "confidence": 0.71,
        "rationale": "After one revision the critique still attacks a simplified version of the claim and offers no vault evidence; no defensible critique was found.",
        "verdict": "shelve",
        "criteria": {
            "engages_actual_claim": {"held": false, "why": "Attacks a stronger universal claim the note never makes."},
            "uses_real_evidence": {"held": false, "why": "Asserts a counterexample without citing any note."},
            "internally_consistent": {"held": true, "why": "The argument does not contradict itself."},
            "has_real_world_adherents": {"held": true, "why": "A skeptic could hold the general position."},
            "concedes_whats_true": {"held": false, "why": "Treats the original as having no merit at all."}
        }
    }"#;

    #[test]
    fn parses_pass_output() {
        let parsed: SteelmanOutput = serde_json::from_str(PASS_OUTPUT).expect("pass JSON parses");
        assert!((parsed.confidence - 0.82).abs() < f32::EPSILON);
        assert_eq!(parsed.verdict, SteelmanVerdict::Pass);
        assert!(parsed.criteria.all_held());
    }

    #[test]
    fn parses_shelve_output_with_failed_criteria() {
        let parsed: SteelmanOutput =
            serde_json::from_str(SHELVE_OUTPUT).expect("shelve JSON parses");
        assert_eq!(parsed.verdict, SteelmanVerdict::Shelve);
        assert!(!parsed.criteria.all_held());
        assert!(!parsed.criteria.engages_actual_claim.held);
        assert!(parsed.criteria.internally_consistent.held);
    }

    #[test]
    fn verdict_enum_parses_all_three_kebab_values() {
        for (raw, expected) in [
            ("pass", SteelmanVerdict::Pass),
            ("request-revision", SteelmanVerdict::RequestRevision),
            ("shelve", SteelmanVerdict::Shelve),
        ] {
            let json = format!("\"{raw}\"");
            let parsed: SteelmanVerdict = serde_json::from_str(&json).expect("verdict parses");
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn hallucinated_verdict_fails_to_parse() {
        // A verdict name outside the three known values must fail,
        // not silently route somewhere (ADR 0016 §5).
        let err = serde_json::from_str::<SteelmanVerdict>("\"maybe\"")
            .expect_err("unknown verdict must fail");
        assert!(err.to_string().contains("maybe") || err.to_string().contains("variant"));
    }

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: SteelmanOutput = serde_json::from_str(PASS_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: SteelmanOutput = serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "verdict": "pass",
            "criteria": {
                "engages_actual_claim": {"held": true, "why": "x"},
                "uses_real_evidence": {"held": true, "why": "x"},
                "internally_consistent": {"held": true, "why": "x"},
                "has_real_world_adherents": {"held": true, "why": "x"},
                "concedes_whats_true": {"held": true, "why": "x"}
            },
            "future_field": "nope"
        }"#;
        let err =
            serde_json::from_str::<SteelmanOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error should point at the offending field; got: {err}"
        );
    }

    #[test]
    fn unknown_criterion_key_rejected() {
        // deny_unknown_fields on the nested CriteriaAssessment: a
        // sixth criterion that isn't one of the five must fail.
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "verdict": "pass",
            "criteria": {
                "engages_actual_claim": {"held": true, "why": "x"},
                "uses_real_evidence": {"held": true, "why": "x"},
                "internally_consistent": {"held": true, "why": "x"},
                "has_real_world_adherents": {"held": true, "why": "x"},
                "concedes_whats_true": {"held": true, "why": "x"},
                "sixth_criterion": {"held": true, "why": "x"}
            }
        }"#;
        let err =
            serde_json::from_str::<SteelmanOutput>(extra).expect_err("extra criterion must fail");
        assert!(err.to_string().contains("sixth_criterion"));
    }

    /// ADR 0011: the JSON the runner streams MUST emit `confidence`
    /// before `rationale`, and both before the `criteria` payload, so
    /// streaming early-exit can abort on the cheap fields.
    #[test]
    fn serializes_confidence_first() {
        let out = SteelmanOutput {
            confidence: 0.9,
            rationale: "r".into(),
            verdict: SteelmanVerdict::Pass,
            criteria: CriteriaAssessment {
                engages_actual_claim: CriterionJudgment {
                    held: true,
                    why: "x".into(),
                },
                uses_real_evidence: CriterionJudgment {
                    held: true,
                    why: "x".into(),
                },
                internally_consistent: CriterionJudgment {
                    held: true,
                    why: "x".into(),
                },
                has_real_world_adherents: CriterionJudgment {
                    held: true,
                    why: "x".into(),
                },
                concedes_whats_true: CriterionJudgment {
                    held: true,
                    why: "x".into(),
                },
            },
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let crit_idx = json.find("\"criteria\"").expect("criteria present");
        assert!(
            conf_idx < rat_idx && rat_idx < crit_idx,
            "field order must be confidence < rationale < criteria \
             (got {conf_idx}, {rat_idx}, {crit_idx}) — ADR 0011 depends on this"
        );
    }
}
