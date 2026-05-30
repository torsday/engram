//! Typed output schema for the Inbox Triage agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/inbox-triage/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the payload fields.
//!
//! Inbox Triage classifies new fleeting notes and proposes downstream
//! routing. Its `max_invasiveness` is `additive` — it only adds
//! frontmatter suggestions; it never modifies note content.
//!
//! ## Confidence interpretation
//!
//! - ≥ 0.70: auto-land the frontmatter suggestion (per `auto_land_min_confidence`)
//! - < 0.70: route to diff-review queue for human approval
//!
//! Unlike Scribe, Inbox Triage has no length-ratio adjustment — its
//! output is a routing recommendation, not a body rewrite, so there
//! is no content-size invariant to enforce.

use serde::{Deserialize, Serialize};

/// Top-level output from the Inbox Triage agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → payload fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageOutput {
    /// Self-assessed confidence (0.0–1.0) in the routing recommendation.
    pub confidence: f32,

    /// One paragraph: the routing decision, citing shape and/or
    /// redundancy evidence where applicable.
    pub rationale: String,

    /// The proposed routing disposition for this fleeting note.
    pub recommended_disposition: Disposition,

    /// Non-empty only when `recommended_disposition` is `MergeInto`.
    /// Each entry names an existing note and gives a one-line reason.
    #[serde(default)]
    pub redundancy_evidence: Vec<RedundancyEntry>,
}

/// The five routing dispositions Inbox Triage can recommend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Standalone thought not yet ready for promotion; keep in the inbox.
    KeepFleeting,
    /// Route through Scribe in literature-mode (named source + quote/summary).
    PromoteLiterature,
    /// Original idea or argument that could become an evergreen note.
    PromoteEvergreenCandidate,
    /// Content substantially overlaps with one or more existing notes.
    /// `redundancy_evidence` must be non-empty when this is selected.
    MergeInto,
    /// No recoverable content (accidental capture, single char, test entry).
    /// **Always a proposal** — never automatically acted upon.
    Discard,
}

/// A single piece of evidence pointing at a note the triaged content
/// should be merged into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedundancyEntry {
    /// Vault-relative path or ULID of the target note.
    pub note_id: String,
    /// One-line description of the content overlap.
    pub reason: String,
}

impl TriageOutput {
    /// Returns `true` if the output is internally consistent.
    ///
    /// A `MergeInto` disposition must have at least one `redundancy_evidence`
    /// entry; all other dispositions must have none (or an empty list).
    pub fn is_consistent(&self) -> bool {
        match self.recommended_disposition {
            Disposition::MergeInto => !self.redundancy_evidence.is_empty(),
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_keep() -> &'static str {
        r#"{
            "confidence": 0.82,
            "rationale": "Short standalone observation with no redundancy. Not yet evergreen.",
            "recommended_disposition": "keep_fleeting",
            "redundancy_evidence": []
        }"#
    }

    fn sample_merge() -> &'static str {
        r#"{
            "confidence": 0.91,
            "rationale": "Content substantially overlaps with existing note on PKM workflows.",
            "recommended_disposition": "merge_into",
            "redundancy_evidence": [
                {"note_id": "pkm/workflows.md", "reason": "Same core claim about spaced repetition"}
            ]
        }"#
    }

    fn sample_discard() -> &'static str {
        r#"{
            "confidence": 0.99,
            "rationale": "Single character capture — no recoverable content.",
            "recommended_disposition": "discard"
        }"#
    }

    #[test]
    fn serde_round_trip_keep() {
        let parsed: TriageOutput = serde_json::from_str(sample_keep()).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: TriageOutput = serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    #[test]
    fn serde_round_trip_merge() {
        let parsed: TriageOutput = serde_json::from_str(sample_merge()).expect("parse");
        assert_eq!(parsed.recommended_disposition, Disposition::MergeInto);
        assert_eq!(parsed.redundancy_evidence.len(), 1);
        assert_eq!(parsed.redundancy_evidence[0].note_id, "pkm/workflows.md");
    }

    #[test]
    fn serde_round_trip_discard_without_evidence_field() {
        // redundancy_evidence has a #[serde(default)] — omitting it is fine.
        let parsed: TriageOutput = serde_json::from_str(sample_discard()).expect("parse");
        assert_eq!(parsed.recommended_disposition, Disposition::Discard);
        assert!(parsed.redundancy_evidence.is_empty());
    }

    #[test]
    fn consistency_merge_requires_evidence() {
        let mut parsed: TriageOutput = serde_json::from_str(sample_merge()).expect("parse");
        assert!(parsed.is_consistent());
        parsed.redundancy_evidence.clear();
        assert!(
            !parsed.is_consistent(),
            "MergeInto with no evidence is inconsistent"
        );
    }

    #[test]
    fn consistency_keep_with_no_evidence_is_fine() {
        let parsed: TriageOutput = serde_json::from_str(sample_keep()).expect("parse");
        assert!(parsed.is_consistent());
    }

    #[test]
    fn all_dispositions_round_trip() {
        let cases = [
            ("keep_fleeting", Disposition::KeepFleeting),
            ("promote_literature", Disposition::PromoteLiterature),
            (
                "promote_evergreen_candidate",
                Disposition::PromoteEvergreenCandidate,
            ),
            ("merge_into", Disposition::MergeInto),
            ("discard", Disposition::Discard),
        ];
        for (s, expected) in cases {
            let json = format!(r#""{}""#, s);
            let parsed: Disposition = serde_json::from_str(&json).expect("parse");
            assert_eq!(parsed, expected, "mismatch for {s}");
            let serialized = serde_json::to_string(&parsed).expect("serialize");
            assert_eq!(serialized, json, "serialization mismatch for {s}");
        }
    }

    #[test]
    fn unknown_disposition_rejected() {
        let bad = r#"{
            "confidence": 0.8,
            "rationale": "r",
            "recommended_disposition": "send_to_archive"
        }"#;
        assert!(
            serde_json::from_str::<TriageOutput>(bad).is_err(),
            "unknown disposition must fail to parse"
        );
    }

    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.8,
            "rationale": "r",
            "recommended_disposition": "keep_fleeting",
            "future_field": true
        }"#;
        let err = serde_json::from_str::<TriageOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error should name the offending field; got: {err}"
        );
    }

    #[test]
    fn confidence_range_not_enforced_by_serde_but_caller_should_clamp() {
        // serde does not enforce 0..=1 range — that's a caller concern.
        let wide = r#"{
            "confidence": 1.5,
            "rationale": "r",
            "recommended_disposition": "keep_fleeting"
        }"#;
        let parsed: TriageOutput = serde_json::from_str(wide).expect("parse");
        assert_eq!(parsed.confidence, 1.5);
    }
}
