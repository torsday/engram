//! Typed output schema for the Pair-Thinking agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/pair-thinking/prompt.md` § "Output schema". Per ADR 0011,
//! `confidence` and `rationale` come first so streaming early-exit
//! can abort generation before the question payload when the
//! agent's confidence in this round's question is below the
//! auto-land floor.
//!
//! Pair-Thinking is conversation-mode (per the [conversation]
//! block in `agents/pair-thinking/config.toml`); each LLM call
//! produces one `PairThinkingTurn` for the current round in a
//! bounded 3–5 round session.

use serde::{Deserialize, Serialize};

/// The five question modes (plus end) Pair-Thinking can produce in
/// a single round.
///
/// The agent picks one mode per round, deliberately — the prompt
/// rejects multi-mode questions because they make both halves
/// weaker. `End` is paired with `should_end: true` and signals the
/// runner to close the session before exhausting the round budget
/// when continuing would hurt more than help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PairThinkingMode {
    /// The paragraph is making a move whose meaning is ambiguous.
    /// Ask the question whose answer is the unambiguous version.
    Clarify,
    /// The paragraph asserts something the rest of the draft
    /// hasn't earned. Ask the question that would either
    /// establish the warrant or reveal it's missing.
    Probe,
    /// The paragraph is reaching for an idea the vault has
    /// already touched. Surface the connection by question rather
    /// than by assertion.
    Connect,
    /// The paragraph is well-written but slightly off the draft's
    /// stated intent. Ask the question that points back at the
    /// real target.
    ReAim,
    /// The session should end after this turn. Paired with
    /// `should_end: true` and an empty `question` string.
    End,
}

/// A single Pair-Thinking turn — one question (or an end signal)
/// for the current round of a bounded 3–5 round session.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `round` → `mode` → `question` →
/// `should_end` → `referenced_note_ids`. The runner can render the
/// "round N of M" UI as soon as `round` arrives and decide whether
/// to abort generation on low confidence before the question
/// itself streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairThinkingTurn {
    /// Self-assessed confidence (0.0–1.0) that this question is
    /// the right one to ask in this round. A low-confidence
    /// question is worse than no question — it wastes a round of
    /// bounded budget. Low confidence is a valid signal to set
    /// `should_end: true`.
    pub confidence: f32,

    /// One paragraph: what made this question promising, and what
    /// could be wrong with asking it.
    pub rationale: String,

    /// The current round number (1-indexed). Echoed back from
    /// the runtime context so the runner can trace alignment
    /// between its bookkeeping and the agent's view of the
    /// session. The runner enforces `1 ≤ round ≤ max_rounds`
    /// post-parse.
    pub round: u32,

    /// The mode picked for this round.
    pub mode: PairThinkingMode,

    /// The single question to deliver to the user's side panel.
    /// Plain text; no markdown. Empty string only when
    /// `should_end == true`.
    pub question: String,

    /// `true` iff the session should close after this turn (the
    /// draft is strong, or the next question would not be worth
    /// the round). Always `true` when `mode == End`.
    pub should_end: bool,

    /// Note IDs the question references. Empty for `clarify` and
    /// `probe`; non-empty for `connect` (the agent must cite the
    /// related vault content the question reaches for).
    #[serde(default)]
    pub referenced_note_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TURN: &str = r#"{
        "confidence": 0.83,
        "rationale": "The paragraph reaches for a connection to information theory without naming it; a connect-mode question can surface 01H8X9 (rate-distortion) which the author has already engaged.",
        "round": 2,
        "mode": "connect",
        "question": "Does the lossy-compression framing here connect to your rate-distortion note (01H8X9), or are you reaching for a different theoretical lineage?",
        "should_end": false,
        "referenced_note_ids": ["01H8X9"]
    }"#;

    const END_TURN: &str = r#"{
        "confidence": 0.92,
        "rationale": "The last two rounds produced sharp clarifications and the draft now reads coherently; further questions would push past the productive frontier.",
        "round": 3,
        "mode": "end",
        "question": "",
        "should_end": true
    }"#;

    #[test]
    fn parses_question_turn() {
        let parsed: PairThinkingTurn =
            serde_json::from_str(SAMPLE_TURN).expect("question turn must parse");
        assert_eq!(parsed.mode, PairThinkingMode::Connect);
        assert_eq!(parsed.round, 2);
        assert!(!parsed.should_end);
        assert_eq!(parsed.referenced_note_ids, vec!["01H8X9".to_string()]);
    }

    #[test]
    fn parses_end_turn() {
        let parsed: PairThinkingTurn =
            serde_json::from_str(END_TURN).expect("end turn must parse");
        assert_eq!(parsed.mode, PairThinkingMode::End);
        assert!(parsed.should_end);
        assert!(parsed.question.is_empty());
        assert!(parsed.referenced_note_ids.is_empty());
    }

    #[test]
    fn round_trips_via_serde_json() {
        for sample in [SAMPLE_TURN, END_TURN] {
            let parsed: PairThinkingTurn = serde_json::from_str(sample).expect("parse");
            let re_serialized = serde_json::to_string(&parsed).expect("serialize");
            let re_parsed: PairThinkingTurn =
                serde_json::from_str(&re_serialized).expect("re-parse");
            assert_eq!(parsed, re_parsed);
        }
    }

    /// Every documented mode parses. If a mode is added to the
    /// prompt but the enum is not updated, this test surfaces the
    /// drift loudly via the missing variant.
    #[test]
    fn all_five_modes_parse() {
        for (json_value, expected) in [
            ("clarify", PairThinkingMode::Clarify),
            ("probe", PairThinkingMode::Probe),
            ("connect", PairThinkingMode::Connect),
            ("re-aim", PairThinkingMode::ReAim),
            ("end", PairThinkingMode::End),
        ] {
            let parsed: PairThinkingMode =
                serde_json::from_str(&format!("\"{json_value}\"")).expect("mode parse");
            assert_eq!(parsed, expected, "round trip for {json_value}");
        }
    }

    /// Unknown modes fail loudly. The runner uses the mode to
    /// decide UI rendering — a silent miss would leave the user
    /// looking at an empty side panel.
    #[test]
    fn unknown_mode_rejected() {
        let bad = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "round": 1,
            "mode": "interrogate",
            "question": "?",
            "should_end": false
        }"#;
        assert!(serde_json::from_str::<PairThinkingTurn>(bad).is_err());
    }

    /// `deny_unknown_fields` is the schema-drift guardrail.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "round": 1,
            "mode": "clarify",
            "question": "?",
            "should_end": false,
            "future_field": "should not parse"
        }"#;
        let err = serde_json::from_str::<PairThinkingTurn>(extra)
            .expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: confidence < rationale
    /// < round < mode < question. The runner streams `round` to
    /// render the "N of M" UI before the question text arrives;
    /// pinning the order here defends against a refactor breaking
    /// that progressive rendering.
    #[test]
    fn serializes_confidence_first() {
        let turn = PairThinkingTurn {
            confidence: 0.9,
            rationale: "r".into(),
            round: 1,
            mode: PairThinkingMode::Clarify,
            question: "?".into(),
            should_end: false,
            referenced_note_ids: vec![],
        };
        let json = serde_json::to_string(&turn).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let round_idx = json.find("\"round\"").expect("round present");
        let mode_idx = json.find("\"mode\"").expect("mode present");
        let q_idx = json.find("\"question\"").expect("question present");
        assert!(
            conf_idx < rat_idx
                && rat_idx < round_idx
                && round_idx < mode_idx
                && mode_idx < q_idx,
            "field order must be confidence < rationale < round < mode < question \
             (got {conf_idx}, {rat_idx}, {round_idx}, {mode_idx}, {q_idx})"
        );
    }

    /// `referenced_note_ids` defaults to empty when omitted —
    /// clarify/probe modes commonly have no references and the
    /// LLM may skip the field entirely.
    #[test]
    fn referenced_note_ids_default_to_empty() {
        let minimal = r#"{
            "confidence": 0.6,
            "rationale": "Probe the unsupported claim in paragraph 2.",
            "round": 1,
            "mode": "probe",
            "question": "What evidence outside this draft would change your mind on the central claim?",
            "should_end": false
        }"#;
        let parsed: PairThinkingTurn = serde_json::from_str(minimal).expect("parse");
        assert!(parsed.referenced_note_ids.is_empty());
    }
}
