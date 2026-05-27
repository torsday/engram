//! Typed output schema for the Witness agent.
//!
//! Witness acknowledges personal and journal notes without analysis,
//! suggestions, or modification to the vault. It is strictly local-only:
//! `model_tier = "fast"` with no cloud escalation, no retrieval, and no
//! memory of prior sessions.
//!
//! Output never lands in the vault. The `acknowledgment` text is written
//! to `.engram/witness/<date>.md` only (tracked by `output_path`). The
//! vault itself is never touched.
//!
//! Per ADR 0011, `confidence` and `rationale` are declared first so
//! streaming early-exit can abort before the acknowledgment payload when
//! confidence is too low.

use serde::{Deserialize, Serialize};

/// Top-level output from the Witness agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → payload fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessOutput {
    /// Self-assessed confidence (0.0–1.0) that the acknowledgment
    /// is appropriate and non-judgmental.
    pub confidence: f32,

    /// One paragraph: why this acknowledgment is appropriate for
    /// the note's content and tone.
    pub rationale: String,

    /// The gentle acknowledgment text. Never stored in the vault —
    /// written to `.engram/witness/<date>.md` only.
    pub acknowledgment: String,

    /// Path where the acknowledgment is written, e.g.
    /// `.engram/witness/2026-05-27.md`. Always under `.engram/witness/`.
    pub output_path: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "confidence": 0.9,
        "rationale": "The note is a brief journal entry; a short acknowledgment is appropriate.",
        "acknowledgment": "Thank you for sharing. It sounds like today was meaningful.",
        "output_path": ".engram/witness/2026-05-27.md"
    }"#;

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: WitnessOutput = serde_json::from_str(MINIMAL).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: WitnessOutput = serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    #[test]
    fn unknown_field_rejected() {
        let extra = r#"{
            "confidence": 0.9,
            "rationale": "r",
            "acknowledgment": "Noted.",
            "output_path": ".engram/witness/2026-01-01.md",
            "future_field": "should not parse"
        }"#;
        let err =
            serde_json::from_str::<WitnessOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error should point at offending field; got: {err}"
        );
    }

    #[test]
    fn missing_acknowledgment_fails() {
        let no_ack = r#"{
            "confidence": 0.9,
            "rationale": "r",
            "output_path": ".engram/witness/2026-01-01.md"
        }"#;
        assert!(
            serde_json::from_str::<WitnessOutput>(no_ack).is_err(),
            "missing acknowledgment must fail"
        );
    }

    #[test]
    fn missing_output_path_fails() {
        let no_path = r#"{
            "confidence": 0.9,
            "rationale": "r",
            "acknowledgment": "Thank you for sharing."
        }"#;
        assert!(
            serde_json::from_str::<WitnessOutput>(no_path).is_err(),
            "missing output_path must fail"
        );
    }

    #[test]
    fn missing_confidence_fails() {
        let no_conf = r#"{
            "rationale": "r",
            "acknowledgment": "Thank you for sharing.",
            "output_path": ".engram/witness/2026-01-01.md"
        }"#;
        assert!(
            serde_json::from_str::<WitnessOutput>(no_conf).is_err(),
            "missing confidence must fail"
        );
    }

    #[test]
    fn missing_rationale_fails() {
        let no_rat = r#"{
            "confidence": 0.9,
            "acknowledgment": "Thank you for sharing.",
            "output_path": ".engram/witness/2026-01-01.md"
        }"#;
        assert!(
            serde_json::from_str::<WitnessOutput>(no_rat).is_err(),
            "missing rationale must fail"
        );
    }

    #[test]
    fn valid_minimal_output_parses() {
        let parsed: WitnessOutput = serde_json::from_str(MINIMAL).expect("minimal must parse");
        assert!((parsed.confidence - 0.9).abs() < f32::EPSILON);
        assert!(!parsed.rationale.is_empty());
        assert!(!parsed.acknowledgment.is_empty());
        assert!(parsed.output_path.starts_with(".engram/witness/"));
    }
}
