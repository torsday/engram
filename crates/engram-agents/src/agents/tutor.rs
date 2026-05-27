//! Tutor agent — spaced-repetition flashcard generation using FSRS-4.5.
//!
//! The Tutor reads evergreen notes and produces flashcards for active
//! recall. It also schedules existing cards using the FSRS-4.5 algorithm,
//! emitting `cards_due` for cards that are overdue or due today.
//!
//! ## Confidence formula
//!
//! `confidence = (llm_score − volume_discount).clamp(0.0, 1.0)`
//!
//! where `volume_discount = (n_cards × 0.01).min(0.2)`.
//!
//! More cards means slightly less certainty about the quality of each;
//! the penalty is capped at 0.20 to avoid collapsing on large note sets.
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "confidence": <0.0–1.0>,
//!   "rationale": "<one paragraph>",
//!   "flashcards": [ { "note_id": "…", "front": "…", "back": "…", "tags": ["…"] } ],
//!   "cards_due": [ { "card_id": "…", "front": "…", "scheduled_date": "YYYY-MM-DD", "days_overdue": <int> } ]
//! }
//! ```

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Full output produced by a single Tutor agent invocation.
///
/// `confidence` and `rationale` come first so providers can
/// stream-and-early-exit per ADR 0011 before generating the
/// potentially large `flashcards` / `cards_due` arrays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TutorOutput {
    /// Self-assessed quality score for this run, adjusted by the
    /// volume discount formula. Range: [0.0, 1.0].
    pub confidence: f32,

    /// One-paragraph rationale for the confidence score and the
    /// flashcard choices made in this run.
    pub rationale: String,

    /// Flashcards generated from evergreen notes in this run.
    /// Empty when no actionable note content was found.
    #[serde(default)]
    pub flashcards: Vec<Flashcard>,

    /// Cards scheduled via FSRS-4.5 that are due or overdue today.
    /// Empty when no review session is pending.
    #[serde(default)]
    pub cards_due: Vec<CardDue>,
}

/// A single spaced-repetition flashcard derived from an evergreen note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flashcard {
    /// Slug or ULID of the source note (from its frontmatter `id` field).
    pub note_id: String,

    /// The question / cue side shown to the learner.
    pub front: String,

    /// The answer side revealed after the learner responds.
    pub back: String,

    /// Tags inherited from the source note or inferred by the agent.
    pub tags: Vec<String>,
}

/// A card that is due for review today (or overdue).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardDue {
    /// Stable card identifier (typically `<note_id>/<card_index>`).
    pub card_id: String,

    /// Front side of the card, displayed in the review queue.
    pub front: String,

    /// ISO-8601 date the FSRS scheduler targeted for this review.
    pub scheduled_date: String,

    /// Days past `scheduled_date` as of the run date. 0 means due
    /// today; negative values indicate future cards (should not
    /// appear in normal output).
    pub days_overdue: i32,
}

// ---------------------------------------------------------------------------
// Confidence formula
// ---------------------------------------------------------------------------

/// Compute Tutor confidence from the raw LLM score and the number
/// of flashcards generated.
///
/// More cards introduce more opportunity for quality variance, so a
/// small per-card penalty is applied, capped at 0.20 total.
///
/// ```text
/// volume_discount = (n_cards × 0.01).min(0.20)
/// confidence      = (llm_score − volume_discount).clamp(0.0, 1.0)
/// ```
pub fn tutor_confidence(llm_score: f32, n_cards: u32) -> f32 {
    let volume_discount = (n_cards as f32 * 0.01).min(0.2);
    (llm_score - volume_discount).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- TutorOutput round-trip ---

    #[test]
    fn tutor_output_round_trip_minimal() {
        let json = r#"{"confidence":0.7,"rationale":"All clear."}"#;
        let out: TutorOutput = serde_json::from_str(json).expect("parse");
        assert_eq!(out.confidence, 0.7);
        assert_eq!(out.rationale, "All clear.");
        assert!(out.flashcards.is_empty());
        assert!(out.cards_due.is_empty());
    }

    #[test]
    fn tutor_output_round_trip_full() {
        let out = TutorOutput {
            confidence: 0.85,
            rationale: "Generated two cards from ownership note.".into(),
            flashcards: vec![Flashcard {
                note_id: "rust-ownership".into(),
                front: "What is ownership in Rust?".into(),
                back: "Each value has a single owner; when the owner goes out of scope the value is dropped.".into(),
                tags: vec!["rust".into(), "memory".into()],
            }],
            cards_due: vec![CardDue {
                card_id: "rust-ownership/0".into(),
                front: "What is ownership in Rust?".into(),
                scheduled_date: "2026-05-27".into(),
                days_overdue: 0,
            }],
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let back: TutorOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(out, back);
    }

    // --- Empty defaults ---

    #[test]
    fn flashcards_and_cards_due_default_to_empty() {
        let json = r#"{"confidence":0.5,"rationale":"r"}"#;
        let out: TutorOutput = serde_json::from_str(json).expect("parse");
        assert!(out.flashcards.is_empty(), "flashcards should default empty");
        assert!(out.cards_due.is_empty(), "cards_due should default empty");
    }

    // --- Unknown field rejection ---

    #[test]
    fn unknown_field_rejected_on_tutor_output() {
        let json = r#"{"confidence":0.5,"rationale":"r","unexpected":"x"}"#;
        serde_json::from_str::<TutorOutput>(json)
            .expect_err("deny_unknown_fields must reject unknown key");
    }

    #[test]
    fn unknown_field_rejected_on_flashcard() {
        let json = r#"{"note_id":"n","front":"f","back":"b","tags":[],"extra":true}"#;
        serde_json::from_str::<Flashcard>(json)
            .expect_err("deny_unknown_fields must reject unknown key on Flashcard");
    }

    // --- Flashcard round-trip ---

    #[test]
    fn flashcard_round_trip() {
        let card = Flashcard {
            note_id: "spaced-repetition".into(),
            front: "What does FSRS stand for?".into(),
            back: "Free Spaced Repetition Scheduler".into(),
            tags: vec!["learning".into()],
        };
        let json = serde_json::to_string(&card).unwrap();
        let back: Flashcard = serde_json::from_str(&json).unwrap();
        assert_eq!(card, back);
    }

    // --- tutor_confidence formula ---

    #[test]
    fn confidence_zero_cards_is_llm_score() {
        assert!((tutor_confidence(0.9, 0) - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn confidence_volume_discount_applied() {
        // 10 cards → discount = 0.10
        let result = tutor_confidence(0.9, 10);
        assert!((result - 0.80).abs() < 1e-5, "got {result}");
    }

    #[test]
    fn confidence_discount_capped_at_0_2() {
        // 100 cards → raw discount = 1.0, but capped at 0.20
        let result = tutor_confidence(0.9, 100);
        assert!((result - 0.70).abs() < 1e-5, "got {result}");
    }

    #[test]
    fn confidence_clamp_lower_bound() {
        // very low llm_score + 20 cards should clamp to 0.0
        let result = tutor_confidence(0.05, 20);
        assert!(result >= 0.0, "must not go negative; got {result}");
        assert!(
            (result - 0.0).abs() < 1e-5,
            "should clamp to 0.0; got {result}"
        );
    }

    #[test]
    fn confidence_clamp_upper_bound() {
        // score above 1.0 must clamp down (defensive)
        let result = tutor_confidence(1.5, 0);
        assert!((result - 1.0).abs() < f32::EPSILON, "got {result}");
    }
}
