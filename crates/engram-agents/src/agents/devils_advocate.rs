//! Typed output schema for the Devil's Advocate agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/devils-advocate/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort generation before the expensive payload
//! when confidence is below the auto-land floor.
//!
//! Devil's Advocate is the critical counterpart to the constructive
//! Steelman (`super::steelman_constructive`). All output passes
//! the Steelman rationality gate (ADR 0007) before counting in
//! council votes or landing as annotations — this struct describes
//! what the LLM emits; whether it counts is a downstream decision.

use serde::{Deserialize, Serialize};

/// Top-level output from the Devil's Advocate agent.
///
/// Field order matters: `confidence`, `rationale`, and `decline`
/// stream first per ADR 0011 so the runner can short-circuit on
/// declines (no payload to validate) or low confidence (gate-
/// rejected) before the expensive `central_claims` / `unstated_
/// assumptions` / `proposed_annotations` / `standalone_critique`
/// payload generates.
///
/// See `agents/devils-advocate/prompt.md` for the prompt that
/// produces this; schema changes in the prompt are schema changes
/// here, in the same PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevilsAdvocateOutput {
    /// Self-assessed confidence (0.0–1.0) that the critique is
    /// defensible (would pass the Steelman rationality gate per
    /// ADR 0007) and useful to the author. Use **0.0** when no
    /// defensible critique exists; pair with `decline: true`.
    pub confidence: f32,

    /// One paragraph: what makes this critique defensible (or why
    /// no defensible critique exists), and what could be wrong
    /// with it. Streams second per ADR 0011.
    pub rationale: String,

    /// `true` iff the note is structurally sound and no defensible
    /// counter-argument exists. A clean decline is a high-quality
    /// output; a forced critique is low-quality and will be
    /// filtered by the gate. When `true`, `proposed_annotations`
    /// is empty and `standalone_critique` is `None`.
    pub decline: bool,

    /// The central claim(s) being critiqued, restated for
    /// precision. Restated claims sit between the original passage
    /// and the critique so the council can verify the critique is
    /// engaging with what the note actually says.
    #[serde(default)]
    pub central_claims: Vec<CentralClaim>,

    /// Assumptions the claim relies on but does not state.
    /// `load_bearing == true` means the central claim collapses
    /// without the assumption — the load-bearing ones are the
    /// council's highest-signal items.
    #[serde(default)]
    pub unstated_assumptions: Vec<UnstatedAssumption>,

    /// HTML-comment markers proposed near the claim being
    /// critiqued. Subject to the Steelman gate before landing.
    #[serde(default)]
    pub proposed_annotations: Vec<ProposedCritiqueAnnotation>,

    /// When the critique warrants a full counter-note (Heretic-
    /// adjacent territory; council-routed), the proposed standalone
    /// note. `None` for inline-only output.
    #[serde(default)]
    pub standalone_critique: Option<StandaloneCritique>,
}

/// A single central claim, quoted verbatim and restated for
/// precision. The runner uses the `quote` to anchor the critique
/// to the note; the `restated_claim` is what subsequent fields
/// argue against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CentralClaim {
    /// Exact text from the note being critiqued.
    pub quote: String,
    /// The claim restated unambiguously — what the critique is
    /// actually engaging with. If the restated form drifts from
    /// the quote, the critique is engaging with a strawman, not
    /// the note.
    pub restated_claim: String,
}

/// An assumption the note's claim relies on but does not state.
///
/// The council pays special attention to `load_bearing == true`
/// assumptions — these are the points where a critique can
/// substantially move the author's thinking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnstatedAssumption {
    /// The assumption, stated plainly.
    pub assumption: String,
    /// `true` iff the central claim fails without this assumption.
    pub load_bearing: bool,
    /// One sentence: why this assumption matters for the claim,
    /// and how the critique engages with it.
    pub why: String,
}

/// A proposed critique annotation — an HTML-comment marker
/// inserted near a passage, citing counter-evidence from the
/// vault.
///
/// Subject to the Steelman gate per ADR 0007 before it lands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedCritiqueAnnotation {
    /// Text in the note that the annotation attaches to.
    pub anchor_text: String,
    /// Surrounding context for disambiguating duplicate anchors.
    pub insertion_context: String,
    /// Note IDs supporting the critique (counter-evidence from the
    /// vault). Every ID must come from the `neighbors` list the
    /// runner provided; fabricated IDs fail validation.
    pub counter_note_ids: Vec<String>,
    /// The critique itself — one or two sentences naming what the
    /// counter-evidence shows.
    pub critique: String,
}

/// A standalone critique note — a full counter-position written as
/// its own note in the vault, council-routed for approval.
///
/// Standalone critiques are Heretic-adjacent (a permanent
/// alternative view) and always go through full council + human
/// approval; this struct describes the proposal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StandaloneCritique {
    /// The proposed title of the counter-note.
    pub proposed_title: String,
    /// The body of the counter-note, written as a sustained
    /// argument against the target's central claim.
    pub body: String,
    /// The note this counter-position is responding to.
    pub target_note_id: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative output JSON matching the prompt's documented
    /// schema. Round-tripping must produce the same value.
    const SAMPLE_OUTPUT: &str = r#"{
        "confidence": 0.84,
        "rationale": "The central claim assumes lossy compression preserves the most important features, but the cited examples are all cases where what counts as 'important' is itself the contested question.",
        "decline": false,
        "central_claims": [
            {
                "quote": "Editing is just lossy compression of intent.",
                "restated_claim": "Editing's value comes from dropping low-importance content while preserving high-importance content."
            }
        ],
        "unstated_assumptions": [
            {
                "assumption": "The editor and the original author agree on which content is high-importance.",
                "load_bearing": true,
                "why": "If they disagree, the editor is not compressing the original intent — they are substituting their own."
            }
        ],
        "proposed_annotations": [
            {
                "anchor_text": "lossy compression of intent",
                "insertion_context": "Editing is just lossy compression of intent.",
                "counter_note_ids": ["01H8X9", "01H8XA"],
                "critique": "01H8X9 explicitly argues editors and authors disagree on importance ranking; the analogy assumes agreement."
            }
        ],
        "standalone_critique": null
    }"#;

    #[test]
    fn parses_representative_output() {
        let parsed: DevilsAdvocateOutput =
            serde_json::from_str(SAMPLE_OUTPUT).expect("sample JSON must parse");
        assert!((parsed.confidence - 0.84).abs() < f32::EPSILON);
        assert!(!parsed.decline);
        assert_eq!(parsed.central_claims.len(), 1);
        assert_eq!(parsed.unstated_assumptions.len(), 1);
        assert!(parsed.unstated_assumptions[0].load_bearing);
        assert!(parsed.standalone_critique.is_none());
    }

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: DevilsAdvocateOutput = serde_json::from_str(SAMPLE_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: DevilsAdvocateOutput =
            serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    /// A clean decline is a high-quality output per the prompt.
    /// The minimal valid form is confidence + rationale + decline,
    /// with empty payload arrays defaulted in.
    #[test]
    fn decline_minimal_form_parses() {
        let decline = r#"{
            "confidence": 0.0,
            "rationale": "The note is structurally sound and no defensible counter-position exists in the vault.",
            "decline": true
        }"#;
        let parsed: DevilsAdvocateOutput =
            serde_json::from_str(decline).expect("minimal decline must parse");
        assert!(parsed.decline);
        assert!(parsed.central_claims.is_empty());
        assert!(parsed.unstated_assumptions.is_empty());
        assert!(parsed.proposed_annotations.is_empty());
        assert!(parsed.standalone_critique.is_none());
    }

    /// `deny_unknown_fields` is the schema-drift guardrail: if the
    /// prompt's schema gains a field, the Rust type must gain it
    /// too. Silent acceptance of unknown fields would hide drift.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "decline": false,
            "future_field": "this should not parse"
        }"#;
        let err = serde_json::from_str::<DevilsAdvocateOutput>(extra)
            .expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: `confidence` must come
    /// before `rationale`, both before `decline`, all before the
    /// payload fields. A refactor reordering struct fields would
    /// silently break the early-exit protocol — this test pins it.
    #[test]
    fn serializes_confidence_first() {
        let out = DevilsAdvocateOutput {
            confidence: 0.9,
            rationale: "r".into(),
            decline: false,
            central_claims: vec![],
            unstated_assumptions: vec![],
            proposed_annotations: vec![],
            standalone_critique: None,
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let dec_idx = json.find("\"decline\"").expect("decline present");
        let claims_idx = json.find("\"central_claims\"").expect("claims present");
        assert!(
            conf_idx < rat_idx && rat_idx < dec_idx && dec_idx < claims_idx,
            "field order must be confidence < rationale < decline < central_claims \
             (got {conf_idx}, {rat_idx}, {dec_idx}, {claims_idx}) — \
             ADR 0011 streaming early-exit depends on this"
        );
    }

    /// `standalone_critique` is optional; a present `null` and a
    /// missing key both deserialize to `None`. The runner relies
    /// on this so prompts can emit either form.
    #[test]
    fn standalone_critique_missing_or_null() {
        let missing = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "decline": false
        }"#;
        let parsed: DevilsAdvocateOutput =
            serde_json::from_str(missing).expect("parse missing");
        assert!(parsed.standalone_critique.is_none());

        let null_value = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "decline": false,
            "standalone_critique": null
        }"#;
        let parsed_null: DevilsAdvocateOutput =
            serde_json::from_str(null_value).expect("parse null");
        assert!(parsed_null.standalone_critique.is_none());

        assert_eq!(parsed, parsed_null);
    }
}
