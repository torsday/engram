//! Typed output schema for the Inquirer agent.
//!
//! Mirrors the JSON schema documented in `agents/inquirer/prompt.md`
//! § "Output schema". Per ADR 0011, `confidence`, `rationale`, and
//! `mode` come first so streaming early-exit can abort generation
//! before the question payload when confidence is below the
//! auto-land floor.
//!
//! ## Mode dispatch is typed at parse time
//!
//! Inquirer consolidates four previously-separate agents into one
//! prompt with four modes selected by trigger. The `mode` field is
//! the runtime dispatch key — and it's a typed [`InquirerMode`]
//! enum here, not a free-form string. An LLM that emits a
//! never-defined mode fails to parse, surfacing the drift loudly
//! rather than letting the runner silently dispatch to a missing
//! handler.
//!
//! This pattern (typed mode enum + serde kebab-case) is the
//! template Voice Keeper (2 modes) and Pair-Thinking (4 question
//! modes + end) reuse in their own typed-output slices.

use serde::{Deserialize, Serialize};

/// The four operating modes Inquirer dispatches on at runtime.
///
/// Per `agents/inquirer/prompt.md`, the runner sets the mode from
/// the trigger context; the agent echoes it back in its output so
/// the runner can verify the LLM understood the trigger. A mode
/// mismatch (the LLM emits a different mode than what was set) is
/// a runtime invariant violation the runner enforces post-parse;
/// this enum just locks out invalid mode strings at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InquirerMode {
    /// End-of-day question connecting today's changes to vault
    /// history. Output: single inbox note. Exactly 1 question.
    DailyReactive,
    /// New empty note just created. 3–5 questions seeded from
    /// semantic neighbors. Marked as prompts; runner auto-deletes
    /// after 48h if the note stays empty.
    SeedEmptyNote,
    /// Weekly cadence. 3–5 questions the vault can't currently
    /// answer (tensions, unexplored intersections, undefended
    /// premises). Output: `questions/YYYY-WNN.md`.
    HolisticGap,
    /// Quarterly negative-space analysis: concepts mentioned but
    /// undeveloped, authors cited but unexamined, domains
    /// adjacent but absent. 5–8 observations framed as questions.
    /// Output: `reflections/blindspots-YYYY-QN.md`.
    Blindspot,
}

/// Top-level output from the Inquirer agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `mode` → `questions` → `output_path`.
/// The runner can decide dispatch (on `mode`) and gate (on
/// `confidence`) before the question payload streams in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InquirerOutput {
    /// Self-assessed confidence (0.0–1.0) that the produced
    /// questions are specific, falsifiable, and worth the
    /// author's attention. Streams first per ADR 0011.
    pub confidence: f32,

    /// One paragraph: what made these questions promising for the
    /// current mode, and what could be wrong with them.
    pub rationale: String,

    /// The mode the agent was invoked in. Must match the trigger
    /// mode the runner set — a mismatch is a runtime invariant
    /// violation the runner enforces post-parse.
    pub mode: InquirerMode,

    /// The generated questions. Count constraints per mode are a
    /// soft schema constraint the runner enforces post-parse:
    /// `DailyReactive` = 1, `SeedEmptyNote` = 3–5,
    /// `HolisticGap` = 3–5, `Blindspot` = 5–8.
    #[serde(default)]
    pub questions: Vec<InquirerQuestion>,

    /// Where the runner will write the output note, derived by
    /// the agent from `mode` + the runtime date/week/quarter
    /// context. The runner validates this path against the
    /// expected per-mode pattern before writing.
    pub output_path: String,
}

/// A single Inquirer question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InquirerQuestion {
    /// The question itself. Specific and falsifiable per the
    /// prompt's constraints; "what about X?" is not a question.
    pub question: String,

    /// Notes that prompted the question. **Empty for
    /// `Blindspot`** observations about absences — there's no
    /// motivating note when the question is about a gap.
    #[serde(default)]
    pub motivating_note_ids: Vec<String>,

    /// One-sentence explanation of why this question is worth
    /// asking at this point in the vault's life. The runner uses
    /// this to render the inbox note's preview.
    pub why_now: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = r#"{
        "confidence": 0.78,
        "rationale": "Today's changes added three notes on lossy compression as a writing metaphor; the question pulls forward an older note on rate-distortion theory that the author hasn't linked from this week's work yet.",
        "mode": "daily-reactive",
        "questions": [
            {
                "question": "Does the lossy-compression metaphor still hold when the editor and author disagree on which signal to drop?",
                "motivating_note_ids": ["01H8X9", "01H8XA", "01H8XB"],
                "why_now": "Three notes from this week reach for the metaphor; an older note (01H8X9) already complicates it without being linked back."
            }
        ],
        "output_path": "inbox/2026-05-27-lossy-compression-disagreement.md"
    }"#;

    #[test]
    fn parses_representative_output() {
        let parsed: InquirerOutput =
            serde_json::from_str(SAMPLE_OUTPUT).expect("sample JSON must parse");
        assert!((parsed.confidence - 0.78).abs() < f32::EPSILON);
        assert_eq!(parsed.mode, InquirerMode::DailyReactive);
        assert_eq!(parsed.questions.len(), 1);
    }

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: InquirerOutput = serde_json::from_str(SAMPLE_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: InquirerOutput =
            serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    /// Every documented mode parses. If a mode is added to the
    /// prompt but the enum is not updated, this test surfaces the
    /// drift loudly via the missing variant.
    #[test]
    fn all_four_modes_parse() {
        for (json_value, expected) in [
            ("daily-reactive", InquirerMode::DailyReactive),
            ("seed-empty-note", InquirerMode::SeedEmptyNote),
            ("holistic-gap", InquirerMode::HolisticGap),
            ("blindspot", InquirerMode::Blindspot),
        ] {
            let parsed: InquirerMode =
                serde_json::from_str(&format!("\"{json_value}\"")).expect("mode parse");
            assert_eq!(parsed, expected, "round trip for {json_value}");
        }
    }

    /// Unknown modes fail loudly — the runner's dispatch table
    /// can't have a silent miss because the LLM hallucinated a
    /// mode name.
    #[test]
    fn unknown_mode_rejected() {
        let bad = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "mode": "yearly-retrospective",
            "questions": [],
            "output_path": "inbox/x.md"
        }"#;
        let err = serde_json::from_str::<InquirerOutput>(bad)
            .expect_err("unknown mode must fail");
        assert!(
            err.to_string().contains("yearly-retrospective")
                || err.to_string().contains("variant"),
            "error message should point at the bad mode; got: {err}"
        );
    }

    /// `deny_unknown_fields` is the schema-drift guardrail.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "mode": "daily-reactive",
            "questions": [],
            "output_path": "inbox/x.md",
            "future_field": "this should not parse"
        }"#;
        let err = serde_json::from_str::<InquirerOutput>(extra)
            .expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: confidence < rationale
    /// < mode < questions. A refactor reordering struct fields
    /// would silently break the early-exit protocol.
    #[test]
    fn serializes_confidence_first() {
        let out = InquirerOutput {
            confidence: 0.9,
            rationale: "r".into(),
            mode: InquirerMode::DailyReactive,
            questions: vec![],
            output_path: "inbox/x.md".into(),
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let mode_idx = json.find("\"mode\"").expect("mode present");
        let q_idx = json.find("\"questions\"").expect("questions present");
        assert!(
            conf_idx < rat_idx && rat_idx < mode_idx && mode_idx < q_idx,
            "field order must be confidence < rationale < mode < questions \
             (got {conf_idx}, {rat_idx}, {mode_idx}, {q_idx})"
        );
    }

    /// Blindspot mode permits questions with empty
    /// `motivating_note_ids` — the question is about an absence,
    /// not a motivating note. The serde default makes this work
    /// even if the LLM omits the field entirely.
    #[test]
    fn blindspot_question_with_empty_motivating_ids() {
        let blindspot = r#"{
            "confidence": 0.7,
            "rationale": "Cited Marshall McLuhan three times but never engaged his media-theory framework.",
            "mode": "blindspot",
            "questions": [
                {
                    "question": "What would McLuhan's tetrad say about your note-taking system?",
                    "why_now": "McLuhan is cited but the tetrad framework is conspicuously absent."
                }
            ],
            "output_path": "reflections/blindspots-2026-Q2.md"
        }"#;
        let parsed: InquirerOutput =
            serde_json::from_str(blindspot).expect("blindspot parse");
        assert_eq!(parsed.mode, InquirerMode::Blindspot);
        assert!(parsed.questions[0].motivating_note_ids.is_empty());
    }
}
