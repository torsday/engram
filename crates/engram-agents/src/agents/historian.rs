//! Typed output schema for the Historian agent.
//!
//! Historian creates weekly activity-log entries summarising what changed in
//! the vault and what the other agents did. It **never** modifies existing
//! notes — every output is a new file written to
//! `meta/activity-log/YYYY-W<nn>.md`.
//!
//! Per ADR 0011, `confidence` and `rationale` are declared first so
//! streaming early-exit can abort before the heavier `log_entry` payload
//! when confidence is too low.

use serde::{Deserialize, Serialize};

/// One line of the agent-activity table inside the weekly log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentActivityLine {
    /// Kebab-case agent name, e.g. `"linker"`.
    pub agent_name: String,
    /// Total number of runs in the week.
    pub runs: u32,
    /// Runs whose output auto-landed (confidence ≥ floor).
    pub auto_lands: u32,
    /// Runs that produced a council proposal.
    pub proposals: u32,
    /// Proposals that were rejected by the user.
    pub rejections: u32,
}

/// Top-level output from the Historian agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → payload fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistorianOutput {
    /// Self-assessed confidence (0.0–1.0) that the log entry is accurate
    /// and complete.
    pub confidence: f32,

    /// One paragraph: why this summary is appropriate for the week's
    /// activity, including any notable events or gaps.
    pub rationale: String,

    /// The full markdown content of the weekly log entry.
    pub log_entry: String,

    /// Vault-relative path where the entry will be written, e.g.
    /// `"meta/activity-log/2026-W22.md"`. Always under
    /// `meta/activity-log/`.
    pub output_path: String,

    /// Per-agent activity rows for the activity table. Empty when no
    /// agents ran during the week.
    #[serde(default)]
    pub agent_activity_summary: Vec<AgentActivityLine>,
}

/// Confidence formula for the Historian agent.
///
/// More events in the week means a more complex summary, so the
/// per-event penalty nudges confidence downward slightly.
///
/// # Parameters
///
/// - `llm_score` — raw confidence emitted by the LLM (0.0–1.0).
/// - `n_events` — number of discrete vault events summarised.
///
/// # Formula
///
/// ```text
/// penalty = min(n_events × 0.005, 0.15)
/// confidence = clamp(llm_score − penalty, 0.0, 1.0)
/// ```
pub fn historian_confidence(llm_score: f32, n_events: u32) -> f32 {
    let penalty = (n_events as f32 * 0.005).min(0.15);
    (llm_score - penalty).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r###"{
        "confidence": 0.8,
        "rationale": "A quiet week with a few note edits.",
        "log_entry": "## 2026-W22\n\nSeven notes edited, two linked.",
        "output_path": "meta/activity-log/2026-W22.md"
    }"###;

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: HistorianOutput = serde_json::from_str(MINIMAL).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: HistorianOutput = serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    #[test]
    fn empty_agent_activity_summary_defaults() {
        let parsed: HistorianOutput = serde_json::from_str(MINIMAL).expect("parse");
        assert!(
            parsed.agent_activity_summary.is_empty(),
            "agent_activity_summary should default to empty vec"
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let extra = r#"{
            "confidence": 0.8,
            "rationale": "r",
            "log_entry": "entry",
            "output_path": "meta/activity-log/2026-W22.md",
            "unexpected_field": true
        }"#;
        let err =
            serde_json::from_str::<HistorianOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("unexpected_field"),
            "error should point at offending field; got: {err}"
        );
    }

    #[test]
    fn confidence_formula_applies_penalty() {
        // 10 events → penalty 0.05
        let c = historian_confidence(0.9, 10);
        assert!((c - 0.85).abs() < 1e-5, "expected 0.85, got {c}");
    }

    #[test]
    fn confidence_formula_clamps_to_zero() {
        // Very low score + large n should not go negative.
        let c = historian_confidence(0.0, 1000);
        assert_eq!(c, 0.0, "must clamp at 0.0");
    }

    #[test]
    fn confidence_formula_penalty_caps_at_015() {
        // 40 events → raw penalty 0.20, capped at 0.15.
        let c = historian_confidence(1.0, 40);
        assert!(
            (c - 0.85).abs() < 1e-5,
            "penalty should cap at 0.15; got confidence {c}"
        );
    }

    #[test]
    fn agent_activity_line_round_trips() {
        let line = AgentActivityLine {
            agent_name: "linker".to_string(),
            runs: 5,
            auto_lands: 3,
            proposals: 2,
            rejections: 1,
        };
        let json = serde_json::to_string(&line).expect("serialize");
        let back: AgentActivityLine = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(line, back);
    }

    #[test]
    fn output_path_is_required() {
        let no_path = r#"{
            "confidence": 0.8,
            "rationale": "r",
            "log_entry": "entry"
        }"#;
        assert!(
            serde_json::from_str::<HistorianOutput>(no_path).is_err(),
            "missing output_path must fail"
        );
    }

    #[test]
    fn valid_output_with_agent_activity_parses() {
        let json = r###"{
            "confidence": 0.82,
            "rationale": "Active week for Linker.",
            "log_entry": "## 2026-W22\n\nLinker ran 5 times.",
            "output_path": "meta/activity-log/2026-W22.md",
            "agent_activity_summary": [
                {
                    "agent_name": "linker",
                    "runs": 5,
                    "auto_lands": 3,
                    "proposals": 2,
                    "rejections": 0
                }
            ]
        }"###;
        let parsed: HistorianOutput = serde_json::from_str(json).expect("parse");
        assert_eq!(parsed.agent_activity_summary.len(), 1);
        assert_eq!(parsed.agent_activity_summary[0].agent_name, "linker");
    }
}
