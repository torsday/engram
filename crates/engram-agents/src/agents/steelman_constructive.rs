//! Typed output schema for the Steelman (constructive role) agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/steelman-constructive/prompt.md` § "Output schema". The
//! prompt instructs the model to emit fields in declaration order
//! (`confidence` first per ADR 0011), so the struct's field order
//! drives both serde's output and the streaming early-exit
//! protocol.
//!
//! ## Round-trip discipline
//!
//! The struct uses owned `String`s and `Vec`s (not borrowed slices)
//! because agent outputs cross thread + tokio-task boundaries
//! between the LLM provider and the runner's invasiveness gate.
//! Borrowing would force lifetime gymnastics for no benefit — the
//! cost of an extra alloc is negligible against the LLM round-trip.

use serde::{Deserialize, Serialize};

/// Top-level output from the Steelman constructive-role agent.
///
/// Field order matters: `confidence` and `rationale` come first so
/// streaming early-exit (per ADR 0011) can abort generation before
/// the expensive `proposed_annotations` / `proposed_reframings`
/// payload when confidence is below the auto-land floor.
///
/// See `agents/steelman-constructive/prompt.md` for the prompt that
/// produces this; the documented JSON schema and this struct must
/// stay in lockstep — a schema change in the prompt is a schema
/// change here, in the same PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteelmanConstructiveOutput {
    /// Self-assessed confidence (0.0–1.0) that the proposed
    /// reframings + annotations are useful. Streams first so the
    /// runner can early-exit on low-confidence outputs before the
    /// payload generates.
    pub confidence: f32,

    /// One paragraph: what made these reframings promising and what
    /// could be wrong. Streams second per ADR 0011.
    pub rationale: String,

    /// HTML-comment markers proposed near the relevant passage.
    /// Each annotation references real note IDs from the
    /// `neighbors` list — never fabricated. Capped at 5 per output
    /// to keep council review tractable.
    #[serde(default)]
    pub proposed_annotations: Vec<ProposedAnnotation>,

    /// Text changes inside existing paragraphs. Each reframing
    /// goes through council review per the invasiveness gate
    /// regardless of confidence. Capped at 3 per output.
    #[serde(default)]
    pub proposed_reframings: Vec<ProposedReframing>,
}

/// A single proposed annotation — an HTML-comment marker inserted
/// near a passage, citing supporting evidence already in the vault.
///
/// Annotations are the auto-landable path (additive; clearly
/// attributed) when the agent's confidence is above the auto-land
/// floor (0.85 per `agents/steelman-constructive/config.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedAnnotation {
    /// The text in the note this annotation attaches to. The
    /// runner uses this to locate the anchor point at write time.
    pub anchor_text: String,

    /// Surrounding text giving the runner enough context to
    /// disambiguate when `anchor_text` appears more than once.
    pub insertion_context: String,

    /// Note IDs supporting the annotation. **Every ID must come
    /// from the `neighbors` list** the runner provided to the
    /// agent — fabricated IDs fail validation and the annotation
    /// is dropped.
    pub supporting_note_ids: Vec<String>,
}

/// A proposed reframing — a text change inside an existing
/// paragraph that sharpens the note's claim toward what the author
/// is reaching for.
///
/// Reframings classify as `editorial` invasiveness regardless of
/// confidence, so they go through council review even when the
/// agent self-rates high.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedReframing {
    /// The exact text in the note that the reframing replaces.
    pub original_excerpt: String,

    /// The sharper rephrasing the agent proposes.
    pub proposed_text: String,

    /// Why this reframing is the strongest version of what the
    /// note is reaching for — one or two sentences. The council
    /// reads this when evaluating the proposal.
    pub rationale: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative output JSON matching the prompt's
    /// documented schema. Round-trip parsing + re-serializing must
    /// produce the same value — that's what locks the Rust type
    /// and the prompt's schema together.
    const SAMPLE_OUTPUT: &str = r#"{
        "confidence": 0.87,
        "rationale": "Two of the neighbor notes establish the load-bearing premise the draft is reaching for; the proposed reframing trades a hedge for the specific claim those notes already support.",
        "proposed_annotations": [
            {
                "anchor_text": "I think this generalizes",
                "insertion_context": "...maybe. I think this generalizes to other lossy-reduction systems.",
                "supporting_note_ids": ["01H8QZ", "01H8RC"]
            }
        ],
        "proposed_reframings": [
            {
                "original_excerpt": "I think this generalizes to other lossy-reduction systems.",
                "proposed_text": "This generalizes to every lossy-reduction system where the reducer chooses what to drop.",
                "rationale": "01H8QZ already commits to the stronger claim; the hedge in the draft is the only weak point."
            }
        ]
    }"#;

    #[test]
    fn parses_representative_output() {
        let parsed: SteelmanConstructiveOutput =
            serde_json::from_str(SAMPLE_OUTPUT).expect("sample JSON must parse");
        assert!((parsed.confidence - 0.87).abs() < f32::EPSILON);
        assert_eq!(parsed.proposed_annotations.len(), 1);
        assert_eq!(parsed.proposed_reframings.len(), 1);
        assert_eq!(
            parsed.proposed_annotations[0].supporting_note_ids,
            vec!["01H8QZ".to_string(), "01H8RC".to_string()]
        );
    }

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: SteelmanConstructiveOutput =
            serde_json::from_str(SAMPLE_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: SteelmanConstructiveOutput =
            serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    /// Empty annotation/reframing arrays are valid — an agent that
    /// finds nothing worth proposing can still emit confidence +
    /// rationale (a clean "no defensible critique" is high-quality
    /// output per the prompt). The `#[serde(default)]` annotation
    /// also lets the arrays be omitted entirely.
    #[test]
    fn missing_arrays_default_to_empty() {
        let minimal = r#"{
            "confidence": 0.12,
            "rationale": "The note is structurally sound; no reframing improves it."
        }"#;
        let parsed: SteelmanConstructiveOutput =
            serde_json::from_str(minimal).expect("minimal JSON must parse");
        assert!(parsed.proposed_annotations.is_empty());
        assert!(parsed.proposed_reframings.is_empty());
    }

    /// `deny_unknown_fields` is the schema-drift guardrail: if the
    /// prompt's schema picks up a new field, the Rust type must
    /// pick it up too. Silent acceptance of unknown fields would
    /// hide that drift.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "future_field": "this should not parse"
        }"#;
        let err = serde_json::from_str::<SteelmanConstructiveOutput>(extra)
            .expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// Field order in serialization is a contract: per ADR 0011 the
    /// JSON the runner streams MUST emit `confidence` before
    /// `rationale`, and both before the payload arrays, so
    /// streaming early-exit can abort on cheap fields. Serde's
    /// default behavior is to emit struct fields in declaration
    /// order, but a refactor that reorders fields would silently
    /// break the streaming contract — this test pins it.
    #[test]
    fn serializes_confidence_first() {
        let out = SteelmanConstructiveOutput {
            confidence: 0.9,
            rationale: "r".into(),
            proposed_annotations: vec![],
            proposed_reframings: vec![],
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let ann_idx = json
            .find("\"proposed_annotations\"")
            .expect("annotations present");
        assert!(
            conf_idx < rat_idx && rat_idx < ann_idx,
            "field order must be confidence < rationale < proposed_annotations \
             (got positions {conf_idx}, {rat_idx}, {ann_idx}) — \
             ADR 0011 streaming early-exit depends on this"
        );
    }
}
