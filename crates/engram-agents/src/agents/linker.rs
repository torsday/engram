//! Typed output schema for the Linker agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/linker/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the per-link proposals.
//!
//! Linker's invasiveness is `additive` per
//! `agents/linker/config.toml` — it only inserts wikilinks into
//! existing note bodies, never rewrites or deletes content.
//! Proposals auto-land when `confidence >= 0.85`; below that
//! threshold they are council-routed for human approval.

use serde::{Deserialize, Serialize};

/// Top-level output from the Linker agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `proposed_links`. Low-confidence
/// outputs short-circuit before any link proposals stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkerOutput {
    /// Self-assessed confidence (0.0–1.0) that the proposed
    /// wikilinks are correct. Low-confidence proposals pollute
    /// the link graph; the prompt requires honest self-rating and
    /// instructs the agent to omit low-signal proposals entirely.
    pub confidence: f32,

    /// One paragraph: what made the proposed links defensible,
    /// and what could be wrong.
    pub rationale: String,

    /// Zero or more proposed wikilinks. An empty list is valid
    /// and common — the agent correctly declines when no
    /// meaningful links are missing.
    #[serde(default)]
    pub proposed_links: Vec<ProposedLink>,
}

/// A single proposed wikilink between two notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedLink {
    /// The note that will receive the new wikilink in its body.
    pub source_note_id: String,

    /// The note being linked to.
    pub target_note_id: String,

    /// The anchor text for the wikilink (e.g. `[[target-note|anchor text]]`).
    pub anchor_text: String,

    /// One-line provenance comment embedded near the link,
    /// e.g. `"by: linker confidence: 0.93"`. Provides auditability
    /// for agent-inserted links per ADR 0005.
    pub provenance_comment: String,

    /// When true, a reciprocal link is also proposed from
    /// `target_note_id` → `source_note_id`. Defaults to false —
    /// most relationships are directional (one note references
    /// the other as context; the reverse link would be noise).
    #[serde(default)]
    pub bidirectional: bool,
}

// ---------------------------------------------------------------------------
// Confidence formula
// ---------------------------------------------------------------------------

/// Compute the Linker confidence score from three independent signals.
///
/// The formula weights LLM self-assessment most heavily (50%),
/// retrieval agreement (semantic neighbors that also link the pair)
/// second (30%), and a calibration adjustment (20%) that corrects for
/// known over- or under-confidence biases observed in the eval corpus.
///
/// All three inputs must be in `[0.0, 1.0]`. The output is clamped to
/// `[0.0, 1.0]` so floating-point drift around the boundary never
/// produces out-of-range values.
///
/// # Arguments
///
/// * `llm_score` — the LLM's own confidence in the link (0–1).
/// * `retrieval_agreement` — fraction of retrieved semantic neighbours
///   that independently suggest the same link (0–1).
/// * `calibration_adjustment` — per-agent calibration factor from the
///   eval corpus; positive shifts up, negative shifts down (0–1 range,
///   0.5 = neutral).
pub fn linker_confidence(
    llm_score: f32,
    retrieval_agreement: f32,
    calibration_adjustment: f32,
) -> f32 {
    (0.5 * llm_score + 0.3 * retrieval_agreement + 0.2 * calibration_adjustment).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- LinkerOutput serde --------------------------------------------------

    #[test]
    fn linker_output_round_trips() {
        let original = LinkerOutput {
            confidence: 0.82,
            rationale:
                "Two notes share the concept of spaced-repetition but neither links the other."
                    .into(),
            proposed_links: vec![ProposedLink {
                source_note_id: "01HAAAA".into(),
                target_note_id: "01HBBBB".into(),
                anchor_text: "spaced repetition".into(),
                provenance_comment: "by: linker confidence: 0.82".into(),
                bidirectional: false,
            }],
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: LinkerOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn proposed_link_bidirectional_defaults_to_false() {
        let json = r#"{
            "source_note_id": "01HAAAA",
            "target_note_id": "01HBBBB",
            "anchor_text": "spaced repetition",
            "provenance_comment": "by: linker confidence: 0.75"
        }"#;
        let link: ProposedLink = serde_json::from_str(json).expect("parse");
        assert!(!link.bidirectional);
    }

    #[test]
    fn empty_proposed_links_defaults_to_empty_vec() {
        let json = r#"{"confidence":0.6,"rationale":"no missing links found"}"#;
        let out: LinkerOutput = serde_json::from_str(json).expect("parse");
        assert!(out.proposed_links.is_empty());
    }

    #[test]
    fn unknown_field_rejected_on_linker_output() {
        let json = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "future_field": "should not parse"
        }"#;
        let err = serde_json::from_str::<LinkerOutput>(json).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error should point at the offending field; got: {err}"
        );
    }

    #[test]
    fn unknown_field_rejected_on_proposed_link() {
        let json = r#"{
            "source_note_id": "a",
            "target_note_id": "b",
            "anchor_text": "x",
            "provenance_comment": "y",
            "extra_field": "nope"
        }"#;
        let err = serde_json::from_str::<ProposedLink>(json).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("extra_field"),
            "error should point at the offending field; got: {err}"
        );
    }

    // -- Confidence formula --------------------------------------------------

    #[test]
    fn linker_confidence_formula_correctness() {
        // 0.5*0.8 + 0.3*0.6 + 0.2*0.7 = 0.40 + 0.18 + 0.14 = 0.72
        let result = linker_confidence(0.8, 0.6, 0.7);
        assert!((result - 0.72).abs() < 1e-5, "expected ~0.72, got {result}");
    }

    #[test]
    fn linker_confidence_clamped_at_max() {
        let result = linker_confidence(1.0, 1.0, 1.0);
        assert_eq!(result, 1.0);
    }

    #[test]
    fn linker_confidence_clamped_at_min() {
        let result = linker_confidence(0.0, 0.0, 0.0);
        assert_eq!(result, 0.0);
    }
}
