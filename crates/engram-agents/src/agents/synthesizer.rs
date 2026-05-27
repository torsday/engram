//! Typed output schema for the Synthesizer agent.
//!
//! Mirrors the JSON schema documented in `agents/synthesizer/prompt.md`
//! § "Output schema". Per ADR 0011, `confidence` and `rationale` come
//! first so streaming early-exit can abort generation before the
//! expensive `proposed_evergreen` payload.
//!
//! Synthesizer's invasiveness is Structural per ADR 0004 — every
//! output is downgraded by the runner's invasiveness gate to a
//! council proposal regardless of `confidence`. This struct
//! describes what the LLM emits; the routing decision is the
//! runner's.

use serde::{Deserialize, Serialize};

/// Top-level output from the Synthesizer agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `decline` → `cluster_coherence` →
/// `proposed_evergreen`. The runner can short-circuit on declines
/// (no payload to validate) or low confidence (gate-rejected)
/// before the evergreen draft generates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthesizerOutput {
    /// Self-assessed confidence (0.0–1.0) that the proposed
    /// evergreen names a real concept the vault is missing and
    /// that the cluster genuinely supports it. Use a low value
    /// when declining or when the proposal is uncertain.
    pub confidence: f32,

    /// One paragraph: what made this cluster recognizable as one
    /// concept, and what could be wrong with the naming.
    pub rationale: String,

    /// `true` iff the cluster does not cohere around a single
    /// concept worth naming. When `true`, `proposed_evergreen` is
    /// `None`.
    pub decline: bool,

    /// Coherence verdict. Even when not declining, the agent may
    /// flag a secondary concept the cluster contains — the runner
    /// can re-cluster around it in the next sweep without losing
    /// the signal that prompted this run.
    pub cluster_coherence: ClusterCoherence,

    /// The proposed evergreen note. `None` when `decline == true`.
    /// Always downgraded by the runner's invasiveness gate to a
    /// council proposal at `.engram/proposals/<id>.json`
    /// regardless of confidence — Synthesizer never auto-lands.
    #[serde(default)]
    pub proposed_evergreen: Option<ProposedEvergreen>,
}

/// Per-cluster coherence verdict.
///
/// `coherent: false` is the decline path; `coherent: true` with
/// `secondary_concept: Some(...)` signals a cluster that contains
/// one strong concept and one weaker one — the runner can re-
/// cluster around the secondary in the next sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterCoherence {
    /// `true` iff the cluster genuinely circles a single concept
    /// worth naming. The agent's `decline` field must be
    /// consistent: `decline == !coherent`.
    pub coherent: bool,

    /// When the cluster splits into two, the second concept the
    /// runner can re-cluster around in the next sweep. `None`
    /// when the cluster is single-concept coherent.
    #[serde(default)]
    pub secondary_concept: Option<String>,
}

/// A proposed evergreen note that names the concept the cluster
/// circles. Always routed through council deliberation + human
/// approval per ADR 0004's Structural invasiveness floor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedEvergreen {
    /// The name of the concept (not a description). Per the
    /// prompt's "name, don't describe" constraint: "Editing as
    /// compression" is a name; "Notes about lossy compression"
    /// is a description.
    pub title: String,

    /// Kebab-case filename per ADR 0006 (pure title-slug
    /// filenames; the ULID lives in frontmatter).
    pub slug: String,

    /// 2–5 paragraphs. Names the concept, distinguishes it from
    /// adjacent concepts in `related_existing_evergreens`, and
    /// links to each source note.
    pub body: String,

    /// Every note in the cluster that supports the proposed
    /// concept. Subset of the cluster the runner provided — the
    /// agent may exclude cluster members that don't support the
    /// chosen concept (especially when `secondary_concept` is
    /// `Some`).
    pub source_note_ids: Vec<String>,

    /// Existing evergreen note IDs the new note should sit beside.
    /// Captures distinguish-from relationships: the new evergreen
    /// is *not* one of these, but should clearly name how it
    /// differs from each.
    #[serde(default)]
    pub related_existing_evergreens: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const PROPOSE_SAMPLE: &str = r#"{
        "confidence": 0.86,
        "rationale": "Five notes from this quarter reach for the same concept — the editor's choice of what to drop — without naming it. The proposed evergreen pulls forward what they all imply.",
        "decline": false,
        "cluster_coherence": {
            "coherent": true,
            "secondary_concept": null
        },
        "proposed_evergreen": {
            "title": "Editing as compression",
            "slug": "editing-as-compression",
            "body": "Editing is the editor's choice of what to drop. The choice is the work — preserving 'intent' is the wrong framing because intent is exactly what the editor and author may disagree on. See 01H8X9 for the rate-distortion analogy.",
            "source_note_ids": ["01H8X9", "01H8XA", "01H8XB", "01H8XC", "01H8XD"],
            "related_existing_evergreens": ["01H7AA", "01H7AB"]
        }
    }"#;

    const DECLINE_SAMPLE: &str = r#"{
        "confidence": 0.0,
        "rationale": "The cluster contains two distinct concepts that share vocabulary (writing-as-compression vs. tape-archive compression); the embedding similarity is coincidental. Re-cluster around the tape-archive notes next sweep.",
        "decline": true,
        "cluster_coherence": {
            "coherent": false,
            "secondary_concept": "tape-archive compression formats"
        }
    }"#;

    #[test]
    fn parses_propose_output() {
        let parsed: SynthesizerOutput =
            serde_json::from_str(PROPOSE_SAMPLE).expect("propose JSON must parse");
        assert!((parsed.confidence - 0.86).abs() < f32::EPSILON);
        assert!(!parsed.decline);
        assert!(parsed.cluster_coherence.coherent);
        let evergreen = parsed.proposed_evergreen.as_ref().expect("present");
        assert_eq!(evergreen.title, "Editing as compression");
        assert_eq!(evergreen.source_note_ids.len(), 5);
        assert_eq!(evergreen.related_existing_evergreens.len(), 2);
    }

    #[test]
    fn parses_decline_output() {
        let parsed: SynthesizerOutput =
            serde_json::from_str(DECLINE_SAMPLE).expect("decline JSON must parse");
        assert!(parsed.decline);
        assert!(!parsed.cluster_coherence.coherent);
        assert_eq!(
            parsed.cluster_coherence.secondary_concept.as_deref(),
            Some("tape-archive compression formats")
        );
        assert!(parsed.proposed_evergreen.is_none());
    }

    #[test]
    fn round_trips_via_serde_json() {
        for sample in [PROPOSE_SAMPLE, DECLINE_SAMPLE] {
            let parsed: SynthesizerOutput = serde_json::from_str(sample).expect("parse");
            let re_serialized = serde_json::to_string(&parsed).expect("serialize");
            let re_parsed: SynthesizerOutput =
                serde_json::from_str(&re_serialized).expect("re-parse");
            assert_eq!(parsed, re_parsed);
        }
    }

    /// `related_existing_evergreens` defaults to empty when the
    /// proposed evergreen sits in unexplored conceptual space —
    /// nothing nearby to distinguish from.
    #[test]
    fn related_existing_evergreens_defaults_empty() {
        let minimal_propose = r#"{
            "confidence": 0.7,
            "rationale": "Cluster names a genuinely new region of the vault.",
            "decline": false,
            "cluster_coherence": { "coherent": true },
            "proposed_evergreen": {
                "title": "New concept",
                "slug": "new-concept",
                "body": "Body text.",
                "source_note_ids": ["01H8AA"]
            }
        }"#;
        let parsed: SynthesizerOutput =
            serde_json::from_str(minimal_propose).expect("parse");
        let ev = parsed.proposed_evergreen.expect("present");
        assert!(ev.related_existing_evergreens.is_empty());
        assert!(parsed.cluster_coherence.secondary_concept.is_none());
    }

    /// `proposed_evergreen` is optional; missing and `null` both
    /// deserialize to `None` (the decline path either omits the
    /// field or sets it to null).
    #[test]
    fn proposed_evergreen_missing_or_null() {
        let missing = r#"{
            "confidence": 0.0,
            "rationale": "decline",
            "decline": true,
            "cluster_coherence": { "coherent": false }
        }"#;
        let parsed: SynthesizerOutput = serde_json::from_str(missing).expect("parse missing");
        assert!(parsed.proposed_evergreen.is_none());

        let null_value = r#"{
            "confidence": 0.0,
            "rationale": "decline",
            "decline": true,
            "cluster_coherence": { "coherent": false },
            "proposed_evergreen": null
        }"#;
        let parsed_null: SynthesizerOutput =
            serde_json::from_str(null_value).expect("parse null");
        assert!(parsed_null.proposed_evergreen.is_none());
        assert_eq!(parsed, parsed_null);
    }

    /// `deny_unknown_fields` is the schema-drift guardrail.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "decline": false,
            "cluster_coherence": { "coherent": true },
            "future_field": "should not parse"
        }"#;
        let err = serde_json::from_str::<SynthesizerOutput>(extra)
            .expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: confidence < rationale <
    /// decline < cluster_coherence < proposed_evergreen. Pinning
    /// this defends against a refactor reordering fields and
    /// silently breaking streaming early-exit.
    #[test]
    fn serializes_confidence_first() {
        let out = SynthesizerOutput {
            confidence: 0.9,
            rationale: "r".into(),
            decline: false,
            cluster_coherence: ClusterCoherence {
                coherent: true,
                secondary_concept: None,
            },
            proposed_evergreen: None,
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let dec_idx = json.find("\"decline\"").expect("decline present");
        let coh_idx = json
            .find("\"cluster_coherence\"")
            .expect("cluster_coherence present");
        assert!(
            conf_idx < rat_idx && rat_idx < dec_idx && dec_idx < coh_idx,
            "field order must be confidence < rationale < decline < cluster_coherence \
             (got {conf_idx}, {rat_idx}, {dec_idx}, {coh_idx})"
        );
    }
}
