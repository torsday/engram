//! Typed output schema for the Merger agent.
//!
//! Mirrors the JSON schema documented in `agents/merger/prompt.md`
//! § "Output schema". Per ADR 0011, `confidence`, `rationale`, and
//! `decline` come first so streaming early-exit can abort before
//! the merge-draft payload.
//!
//! Merger's invasiveness is Structural per ADR 0004 — every output
//! downgrades to a council proposal regardless of confidence.
//!
//! ## Three failure modes encoded at the type level
//!
//! The prompt names three things Merger must avoid:
//!
//! 1. **Silent content loss.** Every claim from either original
//!    must be preserved in the canonical or appear in
//!    [`ProposedMerge::dropped_content`] with a reason.
//! 2. **Silent conflict resolution.** Incompatible claims between
//!    the two originals must surface in
//!    [`ProposedMerge::unresolved_conflicts`] for the council to
//!    decide — never resolved by the agent in the unified body.
//! 3. **Lost incoming links.** Every existing inbound link to
//!    either original must appear in
//!    [`ProposedMerge::link_reassignments`].
//!
//! The runner's post-parse invariants enforce that
//! `dropped_content` and `unresolved_conflicts` are surfaced (the
//! lists may be empty when there's nothing to surface) and that
//! `link_reassignments` covers the union of both originals'
//! incoming links.

use serde::{Deserialize, Serialize};

/// Top-level output from the Merger agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `decline` → `similarity_signals`
/// → `proposed_merge`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergerOutput {
    /// Self-assessed confidence (0.0–1.0) that the two notes are
    /// the same concept and the proposed unification preserves
    /// both originals' content. Confidence floor (0.85 in
    /// `agents/merger/config.toml`) is intentionally a touch
    /// above Splitter's 0.82 because erroneous unification is
    /// harder to back out of than erroneous splitting.
    pub confidence: f32,

    /// One paragraph: what makes these two notes recognizable as
    /// one concept (or why they're adjacent but distinct), and
    /// what could be wrong with the merge.
    pub rationale: String,

    /// `true` iff the notes are adjacent but distinct concepts
    /// that should not be merged. When `true`, `proposed_merge`
    /// is `None`.
    pub decline: bool,

    /// Named signals supporting either the merge or the decline
    /// (e.g. `"shared-citations"`, `"identical-central-claim"`,
    /// `"different-level-of-generality"`,
    /// `"adjacent-but-distinct"`). At least one required; the
    /// runner asserts non-empty post-parse.
    #[serde(default)]
    pub similarity_signals: Vec<String>,

    /// The proposed merge. `None` when `decline == true`.
    #[serde(default)]
    pub proposed_merge: Option<ProposedMerge>,
}

/// A proposed unification of two notes into one canonical note.
///
/// Always council-routed per the Structural invasiveness floor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedMerge {
    /// The unified note that replaces both originals.
    pub canonical: CanonicalNote,

    /// The original titles preserved as aliases pointing at the
    /// canonical note. Both originals' titles must remain
    /// reachable — the prompt's hard constraint.
    #[serde(default)]
    pub aliases: Vec<Alias>,

    /// Every existing inbound link to either original.
    ///
    /// The runner asserts post-parse that this list covers the
    /// union of both originals' incoming-link sets — a missing
    /// link is graph-breaking, exactly the failure mode the
    /// prompt rejects.
    #[serde(default)]
    pub link_reassignments: Vec<LinkReassignment>,

    /// Content from the originals not preserved in the canonical,
    /// each with an explicit reason. Empty is preferred; non-empty
    /// surfaces the cost to the council rather than silently
    /// dropping the content.
    #[serde(default)]
    pub dropped_content: Vec<DroppedContent>,

    /// Incompatible claims between the two originals that the
    /// merge does NOT silently resolve in the canonical body.
    /// The council decides which (if either) survives. Silent
    /// resolution is the failure mode this field exists to
    /// surface.
    #[serde(default)]
    pub unresolved_conflicts: Vec<UnresolvedConflict>,
}

/// The unified canonical note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalNote {
    /// The chosen title — the sharpest of the two originals, or
    /// a new title if neither original's title was strong.
    pub title: String,
    /// Kebab-case filename per ADR 0006.
    pub slug: String,
    /// The unified body. Preserves the best of both originals;
    /// every distinct claim and citation either appears here or
    /// in `dropped_content`.
    pub body: String,
    /// Both originals' IDs. Used by the runner to fold the
    /// originals into aliases at write time.
    pub source_note_ids: Vec<String>,
}

/// An original title preserved as an alias to the canonical note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Alias {
    /// The original note's title.
    pub former_title: String,
    /// The original note's ID.
    pub former_note_id: String,
    /// Kebab-case slug at which the alias lives — usually the
    /// original note's existing slug, sometimes a normalized
    /// form when the original slug was non-conforming.
    pub alias_slug: String,
}

/// A single inbound link the runner will redirect to the canonical
/// note (or to a specific section of it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkReassignment {
    /// The note containing the inbound link.
    pub source_note_id: String,
    /// The anchor text of the link in the source — same source
    /// may link multiple times under different anchors, so the
    /// pair is the addressing unit.
    pub anchor_text: String,
    /// Section heading slug within the canonical when the
    /// whole-note target is too broad to preserve the original
    /// link's intent. Empty string when the canonical itself is
    /// the right destination.
    #[serde(default)]
    pub target_section: String,
}

/// Content from one of the originals that the merge does NOT
/// carry forward into the canonical, with a reason for dropping
/// it.
///
/// Surfaced to the council so the cost of the merge is explicit;
/// the council can override the drop if the content turns out to
/// matter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DroppedContent {
    /// Which original the content came from.
    pub from_note_id: String,
    /// The content being dropped (verbatim quote).
    pub content: String,
    /// One sentence: why this content does not appear in the
    /// canonical. Common reasons: superseded by a sharper
    /// formulation in the other original; tangential to the
    /// unified concept; outdated.
    pub reason: String,
}

/// An incompatible pair of claims between the two originals that
/// the merge does NOT silently resolve.
///
/// The council reads `claim_a` + `claim_b` + `suggested_resolution`
/// and decides which (if either) survives in the canonical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedConflict {
    /// The claim from one original.
    pub claim_a: String,
    /// The incompatible claim from the other.
    pub claim_b: String,
    /// The agent's suggested resolution — one sentence on which
    /// claim is likelier to survive scrutiny, and what evidence
    /// would settle it. The council is not bound by this
    /// suggestion; it's a starting point.
    pub suggested_resolution: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE_SAMPLE: &str = r#"{
        "confidence": 0.88,
        "rationale": "Both notes name the same concept ('editing-as-compression') and reach the same central claim; one is sharper on the rate-distortion analogy, the other on the editor/author disagreement. The canonical preserves both.",
        "decline": false,
        "similarity_signals": ["identical-central-claim", "shared-citations"],
        "proposed_merge": {
            "canonical": {
                "title": "Editing as compression",
                "slug": "editing-as-compression",
                "body": "Editing is the editor's choice of what to drop...",
                "source_note_ids": ["01H8AA", "01H8AB"]
            },
            "aliases": [
                {"former_title": "Editing-as-compression draft", "former_note_id": "01H8AA", "alias_slug": "editing-as-compression-draft"},
                {"former_title": "Lossy reduction in writing", "former_note_id": "01H8AB", "alias_slug": "lossy-reduction-in-writing"}
            ],
            "link_reassignments": [
                {"source_note_id": "01H8X1", "anchor_text": "lossy", "target_section": "rate-distortion-analogy"},
                {"source_note_id": "01H8X2", "anchor_text": "editor-choice", "target_section": ""}
            ],
            "dropped_content": [],
            "unresolved_conflicts": [
                {
                    "claim_a": "Editing always reduces fidelity to original intent.",
                    "claim_b": "Editing can clarify intent that the original prose obscured.",
                    "suggested_resolution": "Claim B is sharper; A is a special case (editor and author disagree on what counts as intent)."
                }
            ]
        }
    }"#;

    const DECLINE_SAMPLE: &str = r#"{
        "confidence": 0.0,
        "rationale": "The notes share vocabulary (both use 'compression') but engage different domains: one is information theory, the other is creative writing. The shared term is coincidental, not concept-equivalent.",
        "decline": true,
        "similarity_signals": ["adjacent-but-distinct", "different-level-of-generality"]
    }"#;

    #[test]
    fn parses_merge_output() {
        let parsed: MergerOutput =
            serde_json::from_str(MERGE_SAMPLE).expect("merge JSON must parse");
        assert!(!parsed.decline);
        let merge = parsed.proposed_merge.as_ref().expect("present");
        assert_eq!(merge.canonical.source_note_ids.len(), 2);
        assert_eq!(merge.aliases.len(), 2);
        assert_eq!(merge.link_reassignments.len(), 2);
        assert!(merge.dropped_content.is_empty());
        assert_eq!(merge.unresolved_conflicts.len(), 1);
        // The targeted-section vs whole-note distinction.
        assert_eq!(
            merge.link_reassignments[0].target_section,
            "rate-distortion-analogy"
        );
        assert!(merge.link_reassignments[1].target_section.is_empty());
    }

    #[test]
    fn parses_decline_output() {
        let parsed: MergerOutput =
            serde_json::from_str(DECLINE_SAMPLE).expect("decline JSON must parse");
        assert!(parsed.decline);
        assert!(parsed.proposed_merge.is_none());
    }

    #[test]
    fn round_trips_via_serde_json() {
        for sample in [MERGE_SAMPLE, DECLINE_SAMPLE] {
            let parsed: MergerOutput = serde_json::from_str(sample).expect("parse");
            let re_serialized = serde_json::to_string(&parsed).expect("serialize");
            let re_parsed: MergerOutput = serde_json::from_str(&re_serialized).expect("re-parse");
            assert_eq!(parsed, re_parsed);
        }
    }

    /// The three "never silent" failure-mode fields all default
    /// to empty when omitted. The agent is expected to emit them
    /// explicitly (the prompt requires it), but the runner
    /// tolerates omission as long as the post-parse invariants
    /// hold (no dropped content not flagged; no inbound links
    /// missing from reassignments; no conflicts the body
    /// silently resolves).
    #[test]
    fn surfacing_fields_default_to_empty() {
        let minimal_merge = r#"{
            "confidence": 0.9,
            "rationale": "Clean merge; no losses, no conflicts.",
            "decline": false,
            "similarity_signals": ["identical-central-claim"],
            "proposed_merge": {
                "canonical": {
                    "title": "X",
                    "slug": "x",
                    "body": "...",
                    "source_note_ids": ["01H1", "01H2"]
                }
            }
        }"#;
        let parsed: MergerOutput = serde_json::from_str(minimal_merge).expect("parse");
        let merge = parsed.proposed_merge.expect("present");
        assert!(merge.aliases.is_empty());
        assert!(merge.link_reassignments.is_empty());
        assert!(merge.dropped_content.is_empty());
        assert!(merge.unresolved_conflicts.is_empty());
    }

    /// `deny_unknown_fields` is the schema-drift guardrail.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "decline": false,
            "similarity_signals": ["x"],
            "future_field": "should not parse"
        }"#;
        let err = serde_json::from_str::<MergerOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract.
    #[test]
    fn serializes_confidence_first() {
        let out = MergerOutput {
            confidence: 0.9,
            rationale: "r".into(),
            decline: false,
            similarity_signals: vec!["x".into()],
            proposed_merge: None,
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("present");
        let rat_idx = json.find("\"rationale\"").expect("present");
        let dec_idx = json.find("\"decline\"").expect("present");
        let sig_idx = json.find("\"similarity_signals\"").expect("present");
        assert!(
            conf_idx < rat_idx && rat_idx < dec_idx && dec_idx < sig_idx,
            "field order must be confidence < rationale < decline < similarity_signals \
             (got {conf_idx}, {rat_idx}, {dec_idx}, {sig_idx})"
        );
    }
}
