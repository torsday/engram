//! Typed output schema for the Confidence Annotator agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/confidence-annotator/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the per-annotation payload.
//!
//! Confidence Annotator's invasiveness is `additive` per
//! `agents/confidence-annotator/config.toml` — it only inserts inline
//! HTML comments into existing evergreen note bodies, never rewrites
//! or deletes prose. Annotations auto-land when `confidence >= 0.80`.

use serde::{Deserialize, Serialize};

/// Top-level output from the Confidence Annotator agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `annotations`. Low-confidence
/// outputs short-circuit before any annotation payload streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceAnnotatorOutput {
    /// Self-assessed confidence (0.0–1.0) that the proposed
    /// annotations are correct. Penalised by 0.02 per annotation
    /// to reflect cumulative uncertainty (see [`annotator_confidence`]).
    pub confidence: f32,

    /// One paragraph: what made the proposed annotations defensible,
    /// and what could be wrong.
    pub rationale: String,

    /// Zero or more proposed inline HTML-comment annotations.
    /// An empty list is valid and common — the agent correctly
    /// declines when every claim already carries an epistemic marker.
    #[serde(default)]
    pub annotations: Vec<ConfidenceAnnotation>,
}

/// A single proposed confidence annotation for a claim in a note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfidenceAnnotation {
    /// The ULID or slug identifying the note that contains the claim.
    pub note_id: String,

    /// Verbatim extract of the claim text that lacks an epistemic marker.
    pub claim_text: String,

    /// A suggested soft marker to add inline, e.g. `"I think"`,
    /// `"likely"`, or `"uncertain"`. The agent does not insert this
    /// directly — it proposes the HTML comment; prose edits remain
    /// the human's decision.
    pub suggested_marker: String,

    /// The inline HTML comment to insert adjacent to the claim,
    /// e.g. `"<!-- confidence: needs-marker -->"`. Provides
    /// auditability for agent-inserted flags per ADR 0005.
    pub html_comment: String,
}

// ---------------------------------------------------------------------------
// Confidence formula
// ---------------------------------------------------------------------------

/// Compute the Confidence Annotator score from the LLM self-assessment
/// and the number of proposed annotations.
///
/// The formula applies a 0.02 penalty per annotation (capped at 0.20)
/// to account for cumulative uncertainty: more flags on a single note
/// means more chances the agent misread prose intent. The result is
/// clamped to `[0.0, 1.0]` so floating-point drift never yields
/// out-of-range values.
///
/// # Examples
///
/// ```
/// use engram_agents::agents::confidence_annotator::annotator_confidence;
///
/// // No annotations: score equals the raw LLM score.
/// assert_eq!(annotator_confidence(0.9, 0), 0.9);
///
/// // Ten annotations: penalty capped at 0.20.
/// assert!((annotator_confidence(0.9, 10) - 0.7_f32).abs() < 1e-5);
/// ```
pub fn annotator_confidence(llm_score: f32, n_annotations: u32) -> f32 {
    let penalty = (n_annotations as f32 * 0.02).min(0.2);
    (llm_score - penalty).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ConfidenceAnnotatorOutput round-trip ----------------------------

    #[test]
    fn output_round_trip_minimal() {
        let json = r#"{"confidence":0.8,"rationale":"looks good"}"#;
        let out: ConfidenceAnnotatorOutput = serde_json::from_str(json).expect("parse");
        assert_eq!(out.confidence, 0.8);
        assert_eq!(out.rationale, "looks good");
        assert!(out.annotations.is_empty());
    }

    #[test]
    fn output_round_trip_with_annotations() {
        let json = r#"{
            "confidence": 0.75,
            "rationale": "two unmarked claims",
            "annotations": [
                {
                    "note_id": "rust-ownership",
                    "claim_text": "Rust is always faster than C++.",
                    "suggested_marker": "I think",
                    "html_comment": "<!-- confidence: needs-marker -->"
                }
            ]
        }"#;
        let out: ConfidenceAnnotatorOutput = serde_json::from_str(json).expect("parse");
        assert_eq!(out.annotations.len(), 1);
        assert_eq!(out.annotations[0].suggested_marker, "I think");
    }

    #[test]
    fn output_empty_annotations_default() {
        let json = r#"{"confidence":0.5,"rationale":"r"}"#;
        let out: ConfidenceAnnotatorOutput = serde_json::from_str(json).expect("parse");
        assert_eq!(out.annotations, Vec::<ConfidenceAnnotation>::new());
    }

    #[test]
    fn output_unknown_field_rejected() {
        let json = r#"{"confidence":0.5,"rationale":"r","unexpected":"x"}"#;
        let err = serde_json::from_str::<ConfidenceAnnotatorOutput>(json)
            .expect_err("deny_unknown_fields must reject");
        assert!(err.to_string().contains("unexpected"));
    }

    // ---- ConfidenceAnnotation round-trip ---------------------------------

    #[test]
    fn annotation_round_trip() {
        let annotation = ConfidenceAnnotation {
            note_id: "my-note".to_string(),
            claim_text: "X causes Y.".to_string(),
            suggested_marker: "likely".to_string(),
            html_comment: "<!-- confidence: needs-marker -->".to_string(),
        };
        let json = serde_json::to_string(&annotation).expect("serialize");
        let back: ConfidenceAnnotation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(annotation, back);
    }

    // ---- annotator_confidence formula ------------------------------------

    #[test]
    fn formula_zero_annotations() {
        // penalty = 0.0, so result equals the raw LLM score exactly.
        assert_eq!(annotator_confidence(0.9, 0), 0.9_f32);
    }

    #[test]
    fn formula_penalty_accumulates() {
        // 5 annotations × 0.02 = 0.10 penalty
        let result = annotator_confidence(0.9, 5);
        assert!(
            (result - 0.8_f32).abs() < 1e-5,
            "expected ~0.80, got {result}"
        );
    }

    #[test]
    fn formula_penalty_capped_at_0_20() {
        // 20 annotations would be 0.40 but cap is 0.20
        let result = annotator_confidence(0.9, 20);
        assert!(
            (result - 0.7_f32).abs() < 1e-5,
            "expected ~0.70, got {result}"
        );
    }

    #[test]
    fn formula_clamps_to_zero() {
        // Even with a very low LLM score and many annotations, result >= 0.0
        assert_eq!(annotator_confidence(0.0, 100), 0.0);
    }

    #[test]
    fn formula_clamps_to_one() {
        // With perfect score and no annotations, result <= 1.0
        assert_eq!(annotator_confidence(1.0, 0), 1.0);
    }
}
