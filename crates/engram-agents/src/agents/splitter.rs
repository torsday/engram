//! Typed output schema for the Splitter agent.
//!
//! Mirrors the JSON schema documented in `agents/splitter/prompt.md`
//! § "Output schema". Per ADR 0011, `confidence`, `rationale`, and
//! `decline` come first so the runner can short-circuit before the
//! resulting-notes + link-redistribution payload streams.
//!
//! Splitter's invasiveness is Structural per ADR 0004 — every
//! output is downgraded by the runner's invasiveness gate to a
//! council proposal regardless of confidence.
//!
//! ## Graph-integrity invariant
//!
//! [`ProposedSplit::unassigned_incoming_links`] must be empty for
//! the proposal to be valid: every inbound link to the original
//! note must land on one of the resulting notes (or on the residual
//! disambiguation note). The struct exposes the field so the
//! agent can surface "I couldn't assign this link" to the council
//! rather than silently dropping it; the runner asserts the
//! `is_empty()` invariant post-parse before writing the proposal.

use serde::{Deserialize, Serialize};

/// Top-level output from the Splitter agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `decline` → `coherence_signals` →
/// `proposed_split`. Declines short-circuit before the payload;
/// low-confidence outputs gate-reject without generating the
/// resulting-notes drafts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitterOutput {
    /// Self-assessed confidence (0.0–1.0) that the note genuinely
    /// violates atomicity and that the proposed split is the right
    /// shape. Used for proposal-quality gating, not auto-land:
    /// every Splitter output is a proposal by construction.
    pub confidence: f32,

    /// One paragraph: what made this note recognizable as
    /// composite (or why no defensible split exists), and what
    /// could be wrong with the proposed split.
    pub rationale: String,

    /// `true` iff the note is coherent and should not be split.
    /// Length alone is not a reason to split — the runner records
    /// the decline so the scheduler skips this note in the next
    /// sweep. When `true`, `proposed_split` is `None`.
    pub decline: bool,

    /// Named signals supporting either the split or the decline
    /// (e.g. `"two-heading-clusters"`,
    /// `"single-sustained-argument"`, `"mid-note-topic-shift"`,
    /// `"continuous-citation-thread"`). At least one signal
    /// required — the prompt rejects evidence-free verdicts.
    /// The runner asserts non-empty post-parse.
    #[serde(default)]
    pub coherence_signals: Vec<String>,

    /// The proposed split. `None` when `decline == true`.
    #[serde(default)]
    pub proposed_split: Option<ProposedSplit>,
}

/// A proposed split of a composite note into 2–3 atomic notes.
///
/// Routed through council deliberation + human approval per ADR
/// 0004's Structural floor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedSplit {
    /// The 2–3 notes the original becomes. Count is a soft
    /// schema constraint enforced by the prompt + the runner
    /// post-parse.
    pub resulting_notes: Vec<ResultingNote>,

    /// What stays at the original path after the split — a
    /// disambiguation note, a redirect, or nothing.
    pub residual: Residual,

    /// Incoming links the agent could not assign to any
    /// resulting note. **Must be empty for the proposal to be
    /// valid** — the council will reject a proposal that drops
    /// graph edges. Non-empty surfaces the unassigned links to
    /// the council without losing them.
    #[serde(default)]
    pub unassigned_incoming_links: Vec<LinkAssignment>,
}

/// One of the 2–3 resulting notes the split produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultingNote {
    /// Title of the new note.
    pub title: String,
    /// Kebab-case filename per ADR 0006.
    pub slug: String,
    /// The body of the new note — the prose moved from the
    /// original's relevant sections plus any new connective text
    /// the agent added.
    pub body: String,
    /// Section IDs from the original note that move into this
    /// resulting note. The runner uses these to validate that
    /// every section of the original is accounted for across all
    /// resulting notes + the residual.
    #[serde(default)]
    pub moved_section_ids: Vec<String>,
    /// Incoming links assigned to this resulting note. The runner
    /// rewrites the originals to point here.
    #[serde(default)]
    pub incoming_link_assignment: Vec<LinkAssignment>,
}

/// A single incoming-link assignment — an existing inbound link
/// the runner will redirect to a resulting note (or surface as
/// unassigned).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkAssignment {
    /// The note that contains the link being redirected.
    pub source_note_id: String,
    /// The anchor text of the link in the source note. The runner
    /// uses this to locate the specific link in `source_note_id`
    /// (the source may link to the original multiple times under
    /// different anchors).
    pub anchor_text: String,
}

/// What remains at the original note's path after the split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Residual {
    /// The kind of residual.
    pub kind: ResidualKind,
    /// The body of the residual. For [`ResidualKind::Disambiguation`]
    /// this is a short note pointing readers at the resulting
    /// notes; for [`ResidualKind::DeleteWithRedirect`] this is the
    /// redirect target slug; for [`ResidualKind::None`] this is
    /// the empty string.
    pub body: String,
}

/// The three shapes the residual can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResidualKind {
    /// A short note at the original path that disambiguates
    /// between the resulting notes. Used when the original title
    /// is itself a meaningful concept the reader might still
    /// search for.
    Disambiguation,
    /// The original is deleted and a redirect is left in its
    /// place. Used when the original title is no longer
    /// meaningful — the split has shown it was a label for a
    /// composite, not a concept.
    DeleteWithRedirect,
    /// Nothing remains. Used when the original was clearly an
    /// inbox-style accumulator and no reader will search for the
    /// original path. Rare — usually a disambiguation or redirect
    /// is the safer choice.
    None,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SPLIT_SAMPLE: &str = r#"{
        "confidence": 0.83,
        "rationale": "The note opens with three paragraphs on rate-distortion theory in information theory and then pivots to editing as compression — two distinct concepts that share vocabulary.",
        "decline": false,
        "coherence_signals": ["two-heading-clusters", "mid-note-topic-shift"],
        "proposed_split": {
            "resulting_notes": [
                {
                    "title": "Rate-distortion theory",
                    "slug": "rate-distortion-theory",
                    "body": "...",
                    "moved_section_ids": ["s1", "s2", "s3"],
                    "incoming_link_assignment": [
                        {"source_note_id": "01H8AA", "anchor_text": "rate-distortion"}
                    ]
                },
                {
                    "title": "Editing as compression",
                    "slug": "editing-as-compression",
                    "body": "...",
                    "moved_section_ids": ["s4", "s5"],
                    "incoming_link_assignment": [
                        {"source_note_id": "01H8AB", "anchor_text": "editing-as-compression"}
                    ]
                }
            ],
            "residual": {
                "kind": "disambiguation",
                "body": "This note was split into [[Rate-distortion theory]] and [[Editing as compression]]."
            },
            "unassigned_incoming_links": []
        }
    }"#;

    const DECLINE_SAMPLE: &str = r#"{
        "confidence": 0.0,
        "rationale": "The note is one sustained argument from premise to conclusion; the three sections build on each other rather than treating distinct concepts.",
        "decline": true,
        "coherence_signals": ["single-sustained-argument", "continuous-citation-thread"]
    }"#;

    #[test]
    fn parses_split_output() {
        let parsed: SplitterOutput =
            serde_json::from_str(SPLIT_SAMPLE).expect("split JSON must parse");
        assert!(!parsed.decline);
        let split = parsed.proposed_split.as_ref().expect("present");
        assert_eq!(split.resulting_notes.len(), 2);
        assert_eq!(split.residual.kind, ResidualKind::Disambiguation);
        assert!(split.unassigned_incoming_links.is_empty());
    }

    #[test]
    fn parses_decline_output() {
        let parsed: SplitterOutput =
            serde_json::from_str(DECLINE_SAMPLE).expect("decline JSON must parse");
        assert!(parsed.decline);
        assert!(parsed.proposed_split.is_none());
        assert_eq!(parsed.coherence_signals.len(), 2);
    }

    #[test]
    fn round_trips_via_serde_json() {
        for sample in [SPLIT_SAMPLE, DECLINE_SAMPLE] {
            let parsed: SplitterOutput = serde_json::from_str(sample).expect("parse");
            let re_serialized = serde_json::to_string(&parsed).expect("serialize");
            let re_parsed: SplitterOutput = serde_json::from_str(&re_serialized).expect("re-parse");
            assert_eq!(parsed, re_parsed);
        }
    }

    /// All three residual kinds round-trip.
    #[test]
    fn all_residual_kinds_parse() {
        for (json_value, expected) in [
            ("disambiguation", ResidualKind::Disambiguation),
            ("delete-with-redirect", ResidualKind::DeleteWithRedirect),
            ("none", ResidualKind::None),
        ] {
            let parsed: ResidualKind =
                serde_json::from_str(&format!("\"{json_value}\"")).expect("parse");
            assert_eq!(parsed, expected);
        }
    }

    /// Surface unassigned links rather than silently dropping
    /// them. The agent populates this when a link's anchor text is
    /// specific enough that none of the resulting notes is a clear
    /// home — the council decides where it lands rather than
    /// Splitter making a low-confidence guess.
    #[test]
    fn surface_unassigned_link() {
        let with_unassigned = r#"{
            "confidence": 0.62,
            "rationale": "Two coherent halves; one inbound link has anchor text that doesn't clearly fit either side.",
            "decline": false,
            "coherence_signals": ["two-heading-clusters"],
            "proposed_split": {
                "resulting_notes": [
                    {"title": "A", "slug": "a", "body": "..."},
                    {"title": "B", "slug": "b", "body": "..."}
                ],
                "residual": {"kind": "delete-with-redirect", "body": "a"},
                "unassigned_incoming_links": [
                    {"source_note_id": "01H8CC", "anchor_text": "the-fuzzy-link"}
                ]
            }
        }"#;
        let parsed: SplitterOutput = serde_json::from_str(with_unassigned).expect("parse");
        let split = parsed.proposed_split.expect("present");
        assert_eq!(split.unassigned_incoming_links.len(), 1);
        // Surfacing the unassigned link is valid output; the
        // runner's post-parse invariant assertion (must be empty
        // for a writable proposal) is separate from this type's
        // parse contract.
    }

    /// `deny_unknown_fields` is the schema-drift guardrail.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "decline": false,
            "coherence_signals": ["x"],
            "future_field": "should not parse"
        }"#;
        let err =
            serde_json::from_str::<SplitterOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: confidence < rationale <
    /// decline < coherence_signals < proposed_split.
    #[test]
    fn serializes_confidence_first() {
        let out = SplitterOutput {
            confidence: 0.9,
            rationale: "r".into(),
            decline: false,
            coherence_signals: vec!["x".into()],
            proposed_split: None,
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("present");
        let rat_idx = json.find("\"rationale\"").expect("present");
        let dec_idx = json.find("\"decline\"").expect("present");
        let sig_idx = json.find("\"coherence_signals\"").expect("present");
        assert!(
            conf_idx < rat_idx && rat_idx < dec_idx && dec_idx < sig_idx,
            "field order must be confidence < rationale < decline < coherence_signals \
             (got {conf_idx}, {rat_idx}, {dec_idx}, {sig_idx})"
        );
    }
}
