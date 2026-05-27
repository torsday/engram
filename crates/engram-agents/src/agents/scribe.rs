//! Typed output schema for the Scribe agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/scribe/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the payload fields.
//!
//! Scribe cleans fleeting notes (voice transcripts, quick captures)
//! and formats literature notes without changing meaning. Its
//! `max_invasiveness` is `editorial` — it rewrites prose but must
//! preserve all authorial meaning.
//!
//! ## Confidence formula
//!
//! The LLM's self-reported confidence is adjusted downward when the
//! `length_ratio` (cleaned chars / original chars) falls outside the
//! expected window for the mode:
//! - `fleeting_cleanup`: 0.8–1.1 is the expected window.
//! - `literature_formatting`: 0.95–1.05 is the expected window.
//!
//! Outside those windows the output is capped at 0.7, signalling
//! that the agent may have dropped content (too short) or added
//! content (too long). The adjustment is a pure function exposed as
//! [`adjusted_confidence`] so callers and tests can verify it
//! independently of serde.

use serde::{Deserialize, Serialize};

/// Top-level output from the Scribe agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → payload fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScribeOutput {
    /// Self-assessed confidence (0.0–1.0) that the cleanup or
    /// formatting is correct and meaning-preserving. The runner
    /// applies [`adjusted_confidence`] to this value before routing.
    pub confidence: f32,

    /// One paragraph: what was changed, why, and what risk of
    /// meaning change (if any) the agent detected.
    pub rationale: String,

    /// The cleaned or formatted note body. Must preserve all
    /// authorial meaning; only voice, filler words, and formatting
    /// may change.
    pub cleaned_body: String,

    /// Optional frontmatter fields the agent proposes to add or
    /// update. Keys are YAML frontmatter field names; values are
    /// JSON-typed. Empty map means no frontmatter changes.
    #[serde(default)]
    pub frontmatter_updates: std::collections::HashMap<String, serde_json::Value>,

    /// Which cleanup mode the agent ran under.
    pub mode: ScribeMode,

    /// `cleaned_body.chars().count() / original_chars`. Used by
    /// [`adjusted_confidence`] to detect silent content drops or
    /// expansions. The LLM computes this from its own output.
    pub length_ratio: f32,
}

/// The two cleanup modes Scribe operates in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScribeMode {
    /// Voice memo or quick-capture cleanup: remove filler words,
    /// fix transcript errors, normalize punctuation. Expected
    /// length ratio 0.8–1.1 (slight compression is normal).
    FleetingCleanup,
    /// Literature note formatting: fix heading levels, normalize
    /// citation style, tighten prose. Expected length ratio
    /// 0.95–1.05 (almost no length change).
    LiteratureFormatting,
}

/// Adjust raw LLM-reported confidence by the `length_ratio` check.
///
/// If the ratio falls outside the mode-appropriate window, the
/// output is capped at 0.7 — the agent may have silently dropped or
/// invented content.
///
/// The result is clamped to `[0.0, 1.0]` regardless of input.
///
/// # Examples
///
/// ```
/// use engram_agents::agents::scribe::{ScribeMode, adjusted_confidence};
///
/// // In-range fleeting: no cap.
/// assert_eq!(adjusted_confidence(0.9, ScribeMode::FleetingCleanup, 1.0), 0.9);
///
/// // Out-of-range fleeting: cap at 0.7.
/// assert_eq!(adjusted_confidence(0.9, ScribeMode::FleetingCleanup, 0.5), 0.7);
/// ```
pub fn adjusted_confidence(base: f32, mode: ScribeMode, length_ratio: f32) -> f32 {
    let in_range = match mode {
        ScribeMode::FleetingCleanup => (0.8..=1.1).contains(&length_ratio),
        ScribeMode::LiteratureFormatting => (0.95..=1.05).contains(&length_ratio),
    };
    let effective = if in_range { base } else { base.min(0.7) };
    effective.clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = r#"{
        "confidence": 0.85,
        "rationale": "Removed filler words and corrected transcript errors. No meaning change detected.",
        "cleaned_body": "The project deadline is next Friday.",
        "frontmatter_updates": {},
        "mode": "fleeting_cleanup",
        "length_ratio": 0.95
    }"#;

    #[test]
    fn serde_round_trip() {
        let parsed: ScribeOutput = serde_json::from_str(SAMPLE_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: ScribeOutput = serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    #[test]
    fn fleeting_ratio_in_range_no_cap() {
        // 0.9 base, ratio 0.95 is within [0.8, 1.1]
        let result = adjusted_confidence(0.9, ScribeMode::FleetingCleanup, 0.95);
        assert_eq!(result, 0.9);
    }

    #[test]
    fn fleeting_ratio_too_low_caps_at_0_7() {
        // ratio 0.5 is below 0.8
        let result = adjusted_confidence(0.9, ScribeMode::FleetingCleanup, 0.5);
        assert_eq!(result, 0.7);
    }

    #[test]
    fn fleeting_ratio_too_high_caps_at_0_7() {
        // ratio 1.5 is above 1.1
        let result = adjusted_confidence(0.9, ScribeMode::FleetingCleanup, 1.5);
        assert_eq!(result, 0.7);
    }

    #[test]
    fn literature_ratio_in_range_no_cap() {
        // ratio 1.0 is within [0.95, 1.05]
        let result = adjusted_confidence(0.88, ScribeMode::LiteratureFormatting, 1.0);
        assert_eq!(result, 0.88);
    }

    #[test]
    fn literature_ratio_slightly_out_caps_at_0_7() {
        // ratio 0.9 is below 0.95
        let result = adjusted_confidence(0.88, ScribeMode::LiteratureFormatting, 0.9);
        assert_eq!(result, 0.7);
    }

    #[test]
    fn confidence_clamped_to_1_even_if_base_exceeds_1() {
        // base > 1.0 should not produce a result > 1.0
        let result = adjusted_confidence(1.5, ScribeMode::FleetingCleanup, 1.0);
        assert_eq!(result, 1.0);
    }

    #[test]
    fn unknown_mode_value_rejected_by_serde() {
        let bad = r#"{
            "confidence": 0.8,
            "rationale": "r",
            "cleaned_body": "x",
            "mode": "invalid_mode",
            "length_ratio": 1.0
        }"#;
        assert!(
            serde_json::from_str::<ScribeOutput>(bad).is_err(),
            "unknown mode value must fail to parse"
        );
    }

    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.8,
            "rationale": "r",
            "cleaned_body": "x",
            "mode": "fleeting_cleanup",
            "length_ratio": 1.0,
            "future_field": "should not parse"
        }"#;
        let err = serde_json::from_str::<ScribeOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    #[test]
    fn frontmatter_updates_defaults_to_empty() {
        let minimal = r#"{
            "confidence": 0.5,
            "rationale": "r",
            "cleaned_body": "x",
            "mode": "literature_formatting",
            "length_ratio": 1.0
        }"#;
        let parsed: ScribeOutput = serde_json::from_str(minimal).expect("parse");
        assert!(parsed.frontmatter_updates.is_empty());
    }
}
