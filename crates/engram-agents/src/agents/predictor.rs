//! Typed output schema for the Predictor agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/predictor/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the prediction payload.
//!
//! The Predictor runs daily (09:00 cron) to surface predictions
//! and confidence claims embedded in notes, track resolution
//! deadlines, and compute Brier-score calibration profiles per
//! topic.

use serde::{Deserialize, Serialize};

/// Top-level output from the Predictor agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → payload arrays. Low-confidence
/// outputs short-circuit before any prediction data streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictorOutput {
    /// Self-assessed confidence (0.0–1.0) that the extracted
    /// predictions and calibration updates are accurate.
    pub confidence: f32,

    /// One paragraph: what predictions were found, why they were
    /// flagged as due (if any), and what calibration data was
    /// updated.
    pub rationale: String,

    /// Predictions newly detected in the scanned notes.
    #[serde(default)]
    pub predictions_found: Vec<PredictionFound>,

    /// Predictions whose due dates have passed and whose outcomes
    /// haven't been recorded yet.
    #[serde(default)]
    pub predictions_due: Vec<PredictionDue>,

    /// Calibration profile updates keyed by topic. Only emitted
    /// when a topic has enough resolved predictions to compute a
    /// meaningful Brier score.
    #[serde(default)]
    pub calibration_updates: Vec<CalibrationUpdate>,
}

/// A prediction or confidence claim found in a note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionFound {
    /// Slug or ULID of the note containing the claim.
    pub note_id: String,

    /// Verbatim (or lightly normalized) text of the claim as it
    /// appears in the note.
    pub claim_text: String,

    /// The confidence stated in the note (0.0–1.0), or `None`
    /// if the claim is implicit (no numeric probability given).
    pub claimed_confidence: Option<f32>,

    /// ISO 8601 date string of the resolution deadline, or `None`
    /// if no date was stated.
    pub due_date: Option<String>,

    /// Short topic label (e.g. "tech", "health", "markets") for
    /// grouping calibration data.
    pub topic: String,
}

/// A prediction whose due date has passed without a recorded
/// outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionDue {
    /// Stable identifier for the prediction in the ledger.
    pub prediction_id: String,

    /// The original claim text.
    pub claim_text: String,

    /// ISO 8601 due date (now in the past).
    pub due_date: String,

    /// How many days past the due date as of the run date.
    /// Zero on the exact due date; positive when overdue.
    pub days_overdue: i32,
}

/// A calibration profile update for a single topic.
///
/// Only emitted when `resolved_count >= min_resolved_for_report`
/// so that sparse data never produces noisy scores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationUpdate {
    /// The topic being calibrated (matches `PredictionFound::topic`).
    pub topic: String,

    /// Mean Brier score for all resolved predictions in this topic
    /// (lower = better; 0.0 = perfect, 1.0 = worst possible).
    pub brier_score: f32,

    /// Number of resolved predictions that went into this score.
    pub resolved_count: u32,

    /// Minimum resolved count before a report is generated.
    /// Typically 10; prevents premature calibration signals.
    pub min_resolved_for_report: u32,
}

// ---------------------------------------------------------------------------
// Confidence formula
// ---------------------------------------------------------------------------

/// Compute the Predictor's confidence score from LLM self-assessment
/// and the number of predictions found in the current batch.
///
/// More predictions found means the agent has more to validate; each
/// additional prediction applies a small penalty, capped at 0.3, to
/// guard against over-confident bulk runs.
///
/// ```
/// # use engram_agents::agents::predictor::predictor_confidence;
/// // No predictions → no penalty; score passes through unchanged.
/// assert!((predictor_confidence(0.8, 0) - 0.8).abs() < f32::EPSILON);
///
/// // 15 predictions → cap kicks in at 0.3.
/// assert!((predictor_confidence(0.9, 15) - 0.6).abs() < 1e-6);
/// ```
pub fn predictor_confidence(llm_score: f32, n_predictions_found: u32) -> f32 {
    let volume_penalty = (n_predictions_found as f32 * 0.02).min(0.3);
    (llm_score - volume_penalty).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- PredictorOutput serde ---

    #[test]
    fn predictor_output_round_trip() {
        let out = PredictorOutput {
            confidence: 0.72,
            rationale: "Found two predictions; one is due.".into(),
            predictions_found: vec![PredictionFound {
                note_id: "tech-forecast-2025".into(),
                claim_text: "Rust will be top-3 systems language by 2026".into(),
                claimed_confidence: Some(0.8),
                due_date: Some("2026-01-01".into()),
                topic: "tech".into(),
            }],
            predictions_due: vec![PredictionDue {
                prediction_id: "pred-001".into(),
                claim_text: "Inflation under 3% by end of year".into(),
                due_date: "2024-12-31".into(),
                days_overdue: 120,
            }],
            calibration_updates: vec![CalibrationUpdate {
                topic: "tech".into(),
                brier_score: 0.12,
                resolved_count: 12,
                min_resolved_for_report: 10,
            }],
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let re_parsed: PredictorOutput = serde_json::from_str(&json).expect("re-parse");
        assert_eq!(out, re_parsed);
    }

    #[test]
    fn empty_arrays_default() {
        let json = r#"{"confidence":0.5,"rationale":"r"}"#;
        let out: PredictorOutput = serde_json::from_str(json).expect("parse");
        assert!(out.predictions_found.is_empty());
        assert!(out.predictions_due.is_empty());
        assert!(out.calibration_updates.is_empty());
    }

    #[test]
    fn unknown_field_rejected() {
        let json = r#"{"confidence":0.5,"rationale":"r","unexpected":"x"}"#;
        assert!(
            serde_json::from_str::<PredictorOutput>(json).is_err(),
            "deny_unknown_fields should reject extra keys"
        );
    }

    // --- PredictionFound ---

    #[test]
    fn prediction_found_none_fields() {
        let json = r#"{
            "note_id": "my-note",
            "claim_text": "X will happen",
            "claimed_confidence": null,
            "due_date": null,
            "topic": "general"
        }"#;
        let p: PredictionFound = serde_json::from_str(json).expect("parse");
        assert!(p.claimed_confidence.is_none());
        assert!(p.due_date.is_none());
    }

    #[test]
    fn prediction_found_with_values() {
        let json = r#"{
            "note_id": "forecast",
            "claim_text": "claim",
            "claimed_confidence": 0.7,
            "due_date": "2025-06-01",
            "topic": "tech"
        }"#;
        let p: PredictionFound = serde_json::from_str(json).expect("parse");
        assert_eq!(p.claimed_confidence, Some(0.7));
        assert_eq!(p.due_date.as_deref(), Some("2025-06-01"));
    }

    // --- predictor_confidence ---

    #[test]
    fn confidence_no_penalty_at_zero_predictions() {
        let score = predictor_confidence(0.8, 0);
        assert!(
            (score - 0.8).abs() < f32::EPSILON,
            "expected 0.8, got {score}"
        );
    }

    #[test]
    fn confidence_penalty_capped_at_0_3() {
        // 15 predictions → 15 * 0.02 = 0.30, exactly at cap.
        let score = predictor_confidence(0.9, 15);
        assert!((score - 0.6).abs() < 1e-6, "expected 0.6, got {score}");
        // 20 predictions → 20 * 0.02 = 0.40, still capped at 0.3.
        let score20 = predictor_confidence(0.9, 20);
        assert!(
            (score20 - 0.6).abs() < 1e-6,
            "cap should hold beyond 15 predictions; got {score20}"
        );
    }

    #[test]
    fn confidence_clamped_to_zero() {
        // llm_score very low + heavy predictions → clamp prevents negative.
        let score = predictor_confidence(0.1, 20);
        assert!(
            score >= 0.0,
            "confidence must not go below 0.0; got {score}"
        );
        assert!((score - 0.0).abs() < 1e-6, "expected 0.0, got {score}");
    }

    // --- CalibrationUpdate ---

    #[test]
    fn calibration_update_round_trip() {
        let update = CalibrationUpdate {
            topic: "markets".into(),
            brier_score: 0.18,
            resolved_count: 14,
            min_resolved_for_report: 10,
        };
        let json = serde_json::to_string(&update).expect("serialize");
        let re_parsed: CalibrationUpdate = serde_json::from_str(&json).expect("re-parse");
        assert_eq!(update, re_parsed);
    }

    /// ADR 0011 streaming-order contract: confidence < rationale
    /// must appear before payload arrays in serialized output.
    #[test]
    fn serializes_confidence_before_rationale_before_payload() {
        let out = PredictorOutput {
            confidence: 0.6,
            rationale: "r".into(),
            predictions_found: vec![],
            predictions_due: vec![],
            calibration_updates: vec![],
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let found_idx = json.find("\"predictions_found\"").expect("found present");
        assert!(
            conf_idx < rat_idx && rat_idx < found_idx,
            "field order must be confidence < rationale < predictions_found \
             (got {conf_idx}, {rat_idx}, {found_idx})"
        );
    }
}
