//! Typed output schema for the Heretic agent.
//!
//! Mirrors the JSON schema documented in `agents/heretic/prompt.md`
//! § "Output schema". Per ADR 0011, `confidence` and `rationale`
//! come first so streaming early-exit can abort generation before
//! the expensive `counter_note` payload when confidence is below the
//! auto-land floor.
//!
//! Heretic is the *sustained* critical agent: where Devil's Advocate
//! (`super::devils_advocate`) raises one-off objections inside a
//! deliberation, Heretic writes a full standalone counter-note
//! (`type: heretical`) that lives in the vault as a permanent
//! challenge — but only when a defensible counter-position genuinely
//! exists, otherwise it shelves. All output passes the Steelman
//! rationality gate (ADR 0007) before landing; this struct describes
//! what the LLM emits, not whether it survives the gate.

use serde::{Deserialize, Serialize};

/// Top-level output from the Heretic agent.
///
/// Field order matters: `confidence`, `rationale`, and `shelved`
/// stream first per ADR 0011 so the runner can short-circuit on
/// shelves (no `counter_note` payload to validate) or low confidence
/// (gate-rejected) before the expensive `counter_note` body
/// generates. `target_note_id` is cheap and known up front (the
/// scheduler picks the note), so it streams before the payload too.
///
/// See `agents/heretic/prompt.md` for the prompt that produces this;
/// schema changes in the prompt are schema changes here, in the same
/// PR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HereticOutput {
    /// Self-assessed confidence (0.0–1.0) that the counter-position
    /// is defensible (would pass the Steelman rationality gate per
    /// ADR 0007) and worth the author's attention. Use **0.0** when
    /// no defensible counter-position exists; pair with
    /// `shelved: true`.
    pub confidence: f32,

    /// One paragraph: what makes this counter-position defensible
    /// (or, when shelving, why no defensible counter-position
    /// exists), and what could be wrong with it. Streams second per
    /// ADR 0011.
    pub rationale: String,

    /// `true` iff the target note is robust and no defensible
    /// counter-position exists. A clean shelve is a high-quality
    /// output — useful evidence the original is sound; a manufactured
    /// heresy is low-quality and gets discarded by the gate. When
    /// `true`, `counter_note` is `None`.
    pub shelved: bool,

    /// The evergreen note being challenged. Always present, including
    /// on a shelve — the shelve record references which note proved
    /// robust.
    pub target_note_id: String,

    /// The drafted heretical note: a sustained counter-argument
    /// written as its own `type: heretical` note, council-routed for
    /// approval and linked bidirectionally with the original.
    /// `None` when `shelved`.
    #[serde(default)]
    pub counter_note: Option<HereticalNote>,
}

/// A standalone heretical note — a full counter-position written as
/// its own note in the vault, council-routed for approval.
///
/// The fields beyond `body` exist to feed the five-criterion Steelman
/// rationality gate (ADR 0007): `engages_with` (criterion 1, engages
/// the actual claim), `counter_evidence` (criterion 2, real
/// evidence), `concedes` (criterion 5, concedes what's true), and
/// `real_world_adherents` (criterion 4, real-world adherents).
/// Internal consistency (criterion 3) is judged from `body`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HereticalNote {
    /// The proposed title of the counter-note, of the form
    /// `Against: <original title>` per the design spec.
    pub proposed_title: String,

    /// The opposing thesis stated in a single sentence.
    pub central_counter_claim: String,

    /// The body of the counter-note, written as a sustained argument
    /// for the opposing position. Becomes the body of the
    /// `type: heretical` note.
    pub body: String,

    /// The original claim(s) the heresy engages, each quoted verbatim
    /// and paired with how the counter-position rebuts that specific
    /// claim. Restated engagement is the gate's check that the heresy
    /// argues against what the note actually says (criterion 1), not
    /// a strawman.
    #[serde(default)]
    pub engages_with: Vec<ClaimEngagement>,

    /// Evidence backing the counter-position — vault note IDs and/or
    /// gated external sources (criterion 2). Every vault ID must come
    /// from the `neighbors` list the runner provided; fabricated IDs
    /// fail validation downstream.
    #[serde(default)]
    pub counter_evidence: Vec<CounterEvidence>,

    /// What the original note gets right (criterion 5). A heresy that
    /// concedes nothing is propaganda, not critique.
    #[serde(default)]
    pub concedes: Vec<String>,

    /// Who actually holds this view — a school of thought, a named
    /// thinker, a tradition (criterion 4). A position nobody credible
    /// holds is not a defensible heresy.
    pub real_world_adherents: String,
}

/// A single point of engagement: the original's claim, quoted
/// verbatim, and how the heretical position counters it.
///
/// If the counter drifts from the quote, the heresy is engaging with
/// a strawman rather than the note — exactly what gate criterion 1
/// rejects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimEngagement {
    /// Exact text from the original note being challenged.
    pub original_quote: String,
    /// How the counter-position rebuts this specific claim.
    pub counter: String,
}

/// A single piece of counter-evidence backing the heretical position.
///
/// Exactly one of `note_id` / `external_url` is set: vault evidence
/// cites a real neighbor ID; external evidence cites a source the
/// runner's gated web search returned. This "exactly one" rule is a
/// runtime invariant (serde enforces the shape, not the mutual
/// exclusion).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterEvidence {
    /// Vault note ID backing the counter-position. Must come from the
    /// `neighbors` list. `None` when this item is external evidence.
    #[serde(default)]
    pub note_id: Option<String>,
    /// External source URL — only when the runner gated web search
    /// open for this run. `None` when this item is vault evidence.
    #[serde(default)]
    pub external_url: Option<String>,
    /// What this evidence establishes for the counter-position.
    pub supports: String,
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
        "confidence": 0.78,
        "rationale": "The note treats spaced repetition as straightforwardly optimal, but a defensible tradition holds that retrieval scheduling crowds out the associative, serendipitous re-encounter that produces genuine insight. The risk is that this conflates two different goals (retention vs. understanding).",
        "shelved": false,
        "target_note_id": "01H8X9ABCD",
        "counter_note": {
            "proposed_title": "Against: Spaced repetition is the optimal learning tool",
            "central_counter_claim": "Optimizing for scheduled retrieval can degrade the unscheduled, associative re-encounter that produces conceptual insight.",
            "body": "The case for spaced repetition rests on retention curves, but retention is not understanding. A note resurfaced on an algorithm's schedule is encountered out of context; a note stumbled upon while chasing an unrelated thread is encountered in a web of live associations...",
            "engages_with": [
                {
                    "original_quote": "Spaced repetition is the optimal learning tool.",
                    "counter": "It is optimal for one objective — durable recall of discrete facts — and actively counterproductive for another the note conflates with it: conceptual synthesis."
                }
            ],
            "counter_evidence": [
                {
                    "note_id": "01H8XAEFGH",
                    "external_url": null,
                    "supports": "An earlier note where the author credits an insight to an unplanned re-reading, not a scheduled review."
                },
                {
                    "note_id": null,
                    "external_url": "https://example.org/desirable-difficulties",
                    "supports": "The desirable-difficulties literature distinguishes retention gains from transfer gains."
                }
            ],
            "concedes": [
                "For discrete factual recall, spaced repetition's evidence base is strong and the note is right about it."
            ],
            "real_world_adherents": "Proponents of constructivist and associative theories of learning; writers in the 'tools for thought' tradition who privilege serendipitous linking over drill."
        }
    }"#;

    #[test]
    fn parses_representative_output() {
        let parsed: HereticOutput =
            serde_json::from_str(SAMPLE_OUTPUT).expect("sample JSON must parse");
        assert!((parsed.confidence - 0.78).abs() < f32::EPSILON);
        assert!(!parsed.shelved);
        assert_eq!(parsed.target_note_id, "01H8X9ABCD");
        let note = parsed.counter_note.expect("counter_note present");
        assert!(note.proposed_title.starts_with("Against: "));
        assert_eq!(note.engages_with.len(), 1);
        assert_eq!(note.counter_evidence.len(), 2);
        // One vault-ID item, one external-URL item.
        assert!(note.counter_evidence[0].note_id.is_some());
        assert!(note.counter_evidence[0].external_url.is_none());
        assert!(note.counter_evidence[1].note_id.is_none());
        assert!(note.counter_evidence[1].external_url.is_some());
        assert_eq!(note.concedes.len(), 1);
    }

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: HereticOutput = serde_json::from_str(SAMPLE_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: HereticOutput = serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    /// A clean shelve ("no defensible counter-position found") is a
    /// high-quality output per the prompt. The minimal valid form is
    /// confidence + rationale + shelved + target_note_id, with
    /// `counter_note` defaulted to `None`.
    #[test]
    fn shelve_minimal_form_parses() {
        let shelve = r#"{
            "confidence": 0.0,
            "rationale": "No defensible counter-position found — the note's claim is narrow, well-evidenced, and the vault contains no contradicting material.",
            "shelved": true,
            "target_note_id": "01H8X9ABCD"
        }"#;
        let parsed: HereticOutput =
            serde_json::from_str(shelve).expect("minimal shelve must parse");
        assert!(parsed.shelved);
        assert_eq!(parsed.target_note_id, "01H8X9ABCD");
        assert!(parsed.counter_note.is_none());
    }

    /// `deny_unknown_fields` is the schema-drift guardrail: if the
    /// prompt's schema gains a field, the Rust type must gain it too.
    /// Silent acceptance of unknown fields would hide drift.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "shelved": true,
            "target_note_id": "01H8X9ABCD",
            "future_field": "this should not parse"
        }"#;
        let err =
            serde_json::from_str::<HereticOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// Nested `deny_unknown_fields` also guards the `counter_note`
    /// payload — drift in `HereticalNote` must fail loudly too.
    #[test]
    fn unknown_field_in_counter_note_rejected() {
        let extra = r#"{
            "confidence": 0.6,
            "rationale": "ok",
            "shelved": false,
            "target_note_id": "01H8X9ABCD",
            "counter_note": {
                "proposed_title": "Against: X",
                "central_counter_claim": "y",
                "body": "z",
                "real_world_adherents": "someone",
                "surprise": "nope"
            }
        }"#;
        let err = serde_json::from_str::<HereticOutput>(extra)
            .expect_err("unknown nested field must fail");
        assert!(
            err.to_string().contains("surprise"),
            "error should point at the nested offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: `confidence` must come
    /// before `rationale`, both before `shelved`, all before
    /// `target_note_id` and the `counter_note` payload. A refactor
    /// reordering struct fields would silently break the early-exit
    /// protocol — this test pins it.
    #[test]
    fn serializes_confidence_first() {
        let out = HereticOutput {
            confidence: 0.9,
            rationale: "r".into(),
            shelved: true,
            target_note_id: "01H8X9ABCD".into(),
            counter_note: None,
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let shelved_idx = json.find("\"shelved\"").expect("shelved present");
        let target_idx = json.find("\"target_note_id\"").expect("target present");
        let note_idx = json.find("\"counter_note\"").expect("counter_note present");
        assert!(
            conf_idx < rat_idx
                && rat_idx < shelved_idx
                && shelved_idx < target_idx
                && target_idx < note_idx,
            "field order must be confidence < rationale < shelved < target_note_id < \
             counter_note (got {conf_idx}, {rat_idx}, {shelved_idx}, {target_idx}, \
             {note_idx}) — ADR 0011 streaming early-exit depends on this"
        );
    }

    /// `counter_note` is optional; a present `null` and a missing key
    /// both deserialize to `None`. The runner relies on this so
    /// prompts can emit either form on a shelve.
    #[test]
    fn counter_note_missing_or_null() {
        let missing = r#"{
            "confidence": 0.0,
            "rationale": "shelved",
            "shelved": true,
            "target_note_id": "01H8X9ABCD"
        }"#;
        let parsed: HereticOutput = serde_json::from_str(missing).expect("parse missing");
        assert!(parsed.counter_note.is_none());

        let null_value = r#"{
            "confidence": 0.0,
            "rationale": "shelved",
            "shelved": true,
            "target_note_id": "01H8X9ABCD",
            "counter_note": null
        }"#;
        let parsed_null: HereticOutput = serde_json::from_str(null_value).expect("parse null");
        assert!(parsed_null.counter_note.is_none());

        assert_eq!(parsed, parsed_null);
    }
}
