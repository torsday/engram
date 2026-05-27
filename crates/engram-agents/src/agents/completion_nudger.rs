//! Completion Nudger — surface unfinished notes as a daily digest.
//!
//! Finds notes with draft status, open TODOs, mid-thought endings, or
//! stale in-progress status. Read-only: never modifies the vault.
//!
//! See `agents/completion-nudger/prompt.md` for the prompt that
//! produces this output.

use serde::{Deserialize, Serialize};

/// Reason a note is surfaced as needing completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NudgeReason {
    /// Frontmatter `status: draft`.
    DraftStatus,
    /// Has unchecked `- [ ]` items.
    OpenTodo,
    /// Ends abruptly with no conclusion (mid-thought).
    MidThought,
    /// `status: in-progress` and untouched for more than 7 days.
    StaleInProgress,
}

/// A single note surfaced by the Completion Nudger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionNudge {
    /// Slug or ULID identifying the note.
    pub note_id: String,
    /// Human-readable note title.
    pub title: String,
    /// Why this note is being nudged.
    pub reason: NudgeReason,
    /// Days since the note was last modified.
    pub days_stale: u32,
    /// First 100 characters of unfinished content.
    pub excerpt: String,
}

/// Full output of one Completion Nudger run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionNudgerOutput {
    /// Overall confidence in the digest (0.0–1.0).
    pub confidence: f32,
    /// One-paragraph rationale explaining the nudges.
    pub rationale: String,
    /// Notes surfaced for completion. Empty when the vault is tidy.
    #[serde(default)]
    pub nudges: Vec<CompletionNudge>,
}

/// Confidence formula for the Completion Nudger.
///
/// More nudges mean more judgment calls, so each nudge beyond the
/// first applies a small penalty, capped at 0.20.
///
/// ```
/// use engram_agents::agents::completion_nudger::nudger_confidence;
///
/// let c = nudger_confidence(0.85, 0);
/// assert!((c - 0.85).abs() < f32::EPSILON);
///
/// let c = nudger_confidence(0.85, 10);
/// assert!(c < 0.85);
/// ```
pub fn nudger_confidence(llm_score: f32, n_nudges: u32) -> f32 {
    let penalty = (n_nudges as f32 * 0.015).min(0.2);
    (llm_score - penalty).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_json() -> &'static str {
        r#"{"confidence":0.75,"rationale":"Two draft notes need attention."}"#
    }

    #[test]
    fn round_trip_empty_nudges() {
        let out: CompletionNudgerOutput = serde_json::from_str(minimal_json()).unwrap();
        assert_eq!(out.confidence, 0.75);
        assert_eq!(out.rationale, "Two draft notes need attention.");
        assert!(out.nudges.is_empty());
        let back = serde_json::to_string(&out).unwrap();
        let out2: CompletionNudgerOutput = serde_json::from_str(&back).unwrap();
        assert_eq!(out, out2);
    }

    #[test]
    fn nudges_default_to_empty_when_absent() {
        let out: CompletionNudgerOutput =
            serde_json::from_str(r#"{"confidence":0.5,"rationale":"r"}"#).unwrap();
        assert!(out.nudges.is_empty());
    }

    #[test]
    fn round_trip_with_nudges() {
        let json = r#"{
            "confidence": 0.8,
            "rationale": "Three notes need attention.",
            "nudges": [
                {
                    "note_id": "rust-notes",
                    "title": "Rust Notes",
                    "reason": "draft_status",
                    "days_stale": 5,
                    "excerpt": "TODO: expand this section..."
                }
            ]
        }"#;
        let out: CompletionNudgerOutput = serde_json::from_str(json).unwrap();
        assert_eq!(out.nudges.len(), 1);
        assert_eq!(out.nudges[0].reason, NudgeReason::DraftStatus);
        assert_eq!(out.nudges[0].days_stale, 5);
    }

    #[test]
    fn unknown_field_rejected() {
        let json = r#"{"confidence":0.5,"rationale":"r","extra_field":"x"}"#;
        let result = serde_json::from_str::<CompletionNudgerOutput>(json);
        assert!(
            result.is_err(),
            "deny_unknown_fields should reject extra_field"
        );
    }

    #[test]
    fn nudge_unknown_field_rejected() {
        let json = r#"{
            "confidence": 0.5,
            "rationale": "r",
            "nudges": [{"note_id":"a","title":"b","reason":"open_todo","days_stale":1,"excerpt":"x","unknown":"y"}]
        }"#;
        let result = serde_json::from_str::<CompletionNudgerOutput>(json);
        assert!(result.is_err());
    }

    #[test]
    fn all_nudge_reason_variants_parse() {
        let cases = [
            ("draft_status", NudgeReason::DraftStatus),
            ("open_todo", NudgeReason::OpenTodo),
            ("mid_thought", NudgeReason::MidThought),
            ("stale_in_progress", NudgeReason::StaleInProgress),
        ];
        for (s, expected) in cases {
            let json = format!(r#""{s}""#);
            let got: NudgeReason = serde_json::from_str(&json).unwrap();
            assert_eq!(got, expected, "variant {s} did not parse correctly");
        }
    }

    #[test]
    fn confidence_formula_no_nudges() {
        let c = nudger_confidence(0.85, 0);
        assert!((c - 0.85).abs() < f32::EPSILON);
    }

    #[test]
    fn confidence_formula_applies_penalty() {
        // 5 nudges × 0.015 = 0.075 penalty
        let c = nudger_confidence(0.80, 5);
        assert!((c - 0.725).abs() < 1e-5, "got {c}");
    }

    #[test]
    fn confidence_formula_clamps_at_zero() {
        // Very low score with many nudges should clamp to 0.0
        let c = nudger_confidence(0.05, 100);
        assert_eq!(c, 0.0);
    }

    #[test]
    fn confidence_formula_clamps_at_one() {
        // Score above 1.0 with no penalty should clamp to 1.0
        let c = nudger_confidence(1.5, 0);
        assert_eq!(c, 1.0);
    }

    #[test]
    fn confidence_formula_penalty_capped_at_0_2() {
        // 20 nudges × 0.015 = 0.3, but penalty is capped at 0.2
        let c20 = nudger_confidence(0.9, 20);
        let c14 = nudger_confidence(0.9, 14); // 14 × 0.015 = 0.21, also capped
                                              // Both should result in the same output since penalty is capped at 0.2
        assert!((c20 - 0.7).abs() < 1e-5, "got {c20}");
        assert!((c14 - 0.7).abs() < 1e-5, "got {c14}");
    }
}
