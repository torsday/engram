//! Typed output schema for the Source Demand agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/source-demand/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the per-claim payload.
//!
//! Source Demand's invasiveness is `additive` per
//! `agents/source-demand/config.toml` — it annotates existing
//! evergreen notes with citation-needed markers, never rewrites
//! or deletes content.

use serde::{Deserialize, Serialize};

/// Top-level output from the Source Demand agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `flagged_claims`. Low-confidence
/// outputs short-circuit before any claim flags stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDemandOutput {
    /// Self-assessed confidence (0.0–1.0) that the flagged claims
    /// are genuine uncited factual assertions. High false-positive
    /// rates degrade trust; the prompt requires honest self-rating.
    pub confidence: f32,

    /// One paragraph: what made the flagging defensible, and what
    /// uncertainty remains (hedged claims, domain ambiguity, etc.).
    pub rationale: String,

    /// Zero or more flagged claims. An empty list is valid and
    /// common — the agent correctly declines when all factual
    /// assertions are already cited or sufficiently hedged.
    #[serde(default)]
    pub flagged_claims: Vec<FlaggedClaim>,
}

/// A single uncited factual claim identified in an evergreen note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlaggedClaim {
    /// The ULID or slug of the note containing the claim.
    pub note_id: String,

    /// The verbatim text of the factual assertion that lacks a
    /// citation. Short excerpts only — enough to locate the claim
    /// without reproducing the whole note.
    pub claim_text: String,

    /// The title of a literature note already in the vault that
    /// could serve as a citation for this claim, if one was found
    /// during retrieval. `None` when no candidate was identified.
    pub suggested_source: Option<String>,

    /// How strongly the claim needs a citation, based on its
    /// linguistic markers and the specificity of the assertion.
    pub severity: ClaimSeverity,
}

/// How urgently a flagged claim needs a citation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimSeverity {
    /// Strong factual claim with no qualifier (e.g. "X causes Y",
    /// "Studies show Z"). Highest priority for citation.
    High,
    /// Likely factual assertion with a weak qualifier (e.g.
    /// "Generally, X leads to Y", "Evidence suggests Z").
    Medium,
    /// Hedged or opinion-like statement (e.g. "It seems X",
    /// "I believe Y"). Lowest priority; flagged only as a reminder.
    Low,
}

// ---------------------------------------------------------------------------
// Confidence formula
// ---------------------------------------------------------------------------

/// Compute the Source Demand confidence score.
///
/// Starts from the LLM's self-assessed score, then applies a
/// per-claim penalty to account for increased risk of false positives
/// as the number of flagged claims grows. More claims = more surface
/// area for errors.
///
/// `llm_score` must be in `[0.0, 1.0]`. `n_claims` is the count of
/// items in `flagged_claims`. The output is clamped to `[0.0, 1.0]`.
pub fn source_demand_confidence(llm_score: f32, n_claims: u32) -> f32 {
    // More claims = more room for error
    let penalty = (n_claims as f32 * 0.03).min(0.25);
    (llm_score - penalty).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_json() -> &'static str {
        r#"{"confidence":0.8,"rationale":"Two strong factual claims found without citations."}"#
    }

    /// Round-trip: serialize then deserialize produces an identical value.
    #[test]
    fn round_trip() {
        let output = SourceDemandOutput {
            confidence: 0.72,
            rationale: "One high-severity claim, no vault source found.".to_string(),
            flagged_claims: vec![FlaggedClaim {
                note_id: "atomic-habits".to_string(),
                claim_text: "Habits form through a four-stage loop.".to_string(),
                suggested_source: None,
                severity: ClaimSeverity::High,
            }],
        };
        let json = serde_json::to_string(&output).unwrap();
        let parsed: SourceDemandOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(output, parsed);
    }

    /// Minimal JSON (no `flagged_claims`) deserializes with an empty Vec
    /// thanks to `#[serde(default)]`.
    #[test]
    fn empty_claims_default() {
        let output: SourceDemandOutput = serde_json::from_str(minimal_json()).unwrap();
        assert!(output.flagged_claims.is_empty());
        assert!((output.confidence - 0.8).abs() < f32::EPSILON);
    }

    /// `deny_unknown_fields` rejects JSON with extra keys.
    #[test]
    fn unknown_field_rejected() {
        let json = r#"{"confidence":0.5,"rationale":"r","unknown_key":"x"}"#;
        let err = serde_json::from_str::<SourceDemandOutput>(json).unwrap_err();
        assert!(
            err.to_string().contains("unknown_key"),
            "error should mention the offending field; got: {err}"
        );
    }

    /// All three `ClaimSeverity` variants parse from their snake_case names.
    #[test]
    fn claim_severity_high_parses() {
        let s: ClaimSeverity = serde_json::from_str(r#""high""#).unwrap();
        assert_eq!(s, ClaimSeverity::High);
    }

    #[test]
    fn claim_severity_medium_parses() {
        let s: ClaimSeverity = serde_json::from_str(r#""medium""#).unwrap();
        assert_eq!(s, ClaimSeverity::Medium);
    }

    #[test]
    fn claim_severity_low_parses() {
        let s: ClaimSeverity = serde_json::from_str(r#""low""#).unwrap();
        assert_eq!(s, ClaimSeverity::Low);
    }

    /// `FlaggedClaim` with `suggested_source: None` round-trips correctly.
    #[test]
    fn flagged_claim_none_suggested_source() {
        let claim = FlaggedClaim {
            note_id: "some-note".to_string(),
            claim_text: "X causes Y.".to_string(),
            suggested_source: None,
            severity: ClaimSeverity::High,
        };
        let json = serde_json::to_string(&claim).unwrap();
        let parsed: FlaggedClaim = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.suggested_source, None);
    }

    /// Confidence formula basic case: penalty is 3% per claim.
    #[test]
    fn confidence_formula_basic() {
        let result = source_demand_confidence(0.90, 3);
        let expected = 0.90 - 3.0 * 0.03;
        assert!(
            (result - expected).abs() < 1e-6,
            "got {result}, expected {expected}"
        );
    }

    /// Penalty is capped at 0.25 regardless of claim count.
    #[test]
    fn confidence_formula_penalty_capped() {
        // 10 claims × 0.03 = 0.30 > 0.25 cap
        let result = source_demand_confidence(0.90, 10);
        let expected = 0.90 - 0.25;
        assert!(
            (result - expected).abs() < 1e-6,
            "got {result}, expected {expected}"
        );
    }

    /// Result is clamped to 0.0 when penalty exceeds llm_score.
    #[test]
    fn confidence_formula_clamps_to_zero() {
        let result = source_demand_confidence(0.10, 5);
        assert!(result >= 0.0);
        assert!((result - 0.0).abs() < 1e-6);
    }

    /// Result is clamped to 1.0 when llm_score is 1.0 and there are no claims.
    #[test]
    fn confidence_formula_clamps_to_one() {
        let result = source_demand_confidence(1.0, 0);
        assert!((result - 1.0).abs() < 1e-6);
    }
}
