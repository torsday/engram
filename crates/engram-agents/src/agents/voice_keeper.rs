//! Typed output schema for the Voice Keeper agent.
//!
//! Mirrors the JSON schema documented in `agents/voice-keeper/prompt.md`
//! § "Output schema". Per ADR 0011, `confidence`, `rationale`, and
//! `mode` come first so the runner can short-circuit on
//! low-confidence outputs before the payload streams.
//!
//! Reuses the [`super::inquirer::InquirerMode`] template (typed
//! enum + serde kebab-case) — Voice Keeper's two-mode case is the
//! minimal variant of the multi-mode pattern Inquirer established.

use serde::{Deserialize, Serialize};

/// The two modes Voice Keeper operates in.
///
/// The prompt's mode/payload schema makes [`Verdict`]s exclusive
/// to `Review` and [`ModelUpdate`] exclusive to `ModelUpdateMode`.
/// The Rust struct carries both as conditional fields (one is
/// always empty/None for any given output); the runner enforces
/// mode/payload consistency as a post-parse invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceKeeperMode {
    /// Drafted content from another agent is being evaluated by a
    /// council. Voice Keeper produces per-passage verdicts and
    /// optional rewrite proposals.
    Review,
    /// Monthly cadence. Voice Keeper reads recent author-written
    /// notes and produces a proposed update to the voice model
    /// at `.engram/meta/voice-model.md`. Always human-approved.
    ModelUpdate,
}

/// Top-level output from the Voice Keeper agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `mode` → payload fields. Mode
/// streams before the payload so the runner can route the output
/// without parsing the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceKeeperOutput {
    /// Self-assessed confidence (0.0–1.0) that the voice verdicts
    /// are accurate and any proposed rewrites preserve the
    /// drafting agent's meaning.
    pub confidence: f32,

    /// One paragraph: what voice signals the draft hits or
    /// misses, and what could be wrong with this read.
    pub rationale: String,

    /// Which mode the agent was invoked in. The runner uses this
    /// to validate that the payload (verdicts vs. model_update)
    /// matches expectation, and to route the output.
    pub mode: VoiceKeeperMode,

    /// Per-passage verdicts. Populated only when
    /// `mode == VoiceKeeperMode::Review`. Empty under
    /// `mode == VoiceKeeperMode::ModelUpdate` — the runner
    /// enforces this consistency post-parse.
    #[serde(default)]
    pub verdicts: Vec<VerdictItem>,

    /// Proposed voice-model update. Populated only when
    /// `mode == VoiceKeeperMode::ModelUpdate`. Always a proposal —
    /// the human approves before it becomes the new reference.
    #[serde(default)]
    pub model_update: Option<ModelUpdate>,
}

/// A single per-passage verdict produced in `review` mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictItem {
    /// The passage being judged, quoted from the draft.
    pub passage_excerpt: String,

    /// The verdict for this passage.
    pub verdict: PassageVerdict,

    /// Named voice signals supporting the verdict (e.g.
    /// `"opens-abstract"`, `"utilize-not-use"`,
    /// `"em-dash-stack"`). Vague "doesn't sound like you" is not
    /// a valid verdict — the prompt rejects it; the runner
    /// rejects passages with empty `voice_signals` post-parse.
    #[serde(default)]
    pub voice_signals: Vec<String>,

    /// Suggested rewrite that preserves the drafting agent's
    /// meaning while restoring the user's voice. `None` for
    /// `Pass` and `Flag` verdicts; `Some` for `ProposeRewrite`.
    /// The runner enforces this consistency post-parse.
    #[serde(default)]
    pub proposed_rewrite: Option<String>,
}

/// The three per-passage verdicts Voice Keeper can issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassageVerdict {
    /// Sounds like the user. No action.
    Pass,
    /// Doesn't sound like the user; the named voice signals
    /// explain why. No rewrite proposed (the drafting agent or
    /// the council decides whether to fix).
    Flag,
    /// Voice Keeper proposes a specific rewrite that restores
    /// the user's voice while preserving the drafting agent's
    /// meaning. `proposed_rewrite` must be `Some`.
    ProposeRewrite,
}

/// A proposed update to the voice model at
/// `.engram/meta/voice-model.md`. Always a human-approved
/// proposal — voice drift should be acknowledged deliberately,
/// not absorbed silently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUpdate {
    /// Voice patterns the recent author-written notes added.
    /// Each entry is a short named signal like
    /// `"prefers-em-dash-over-colon"`.
    #[serde(default)]
    pub additions: Vec<String>,

    /// Voice patterns previously in the model that no longer
    /// appear in recent author-written notes. The human decides
    /// whether to retire them; Voice Keeper just surfaces the
    /// drift.
    #[serde(default)]
    pub retirements: Vec<String>,

    /// One paragraph: why this update is the right change, and
    /// what could be wrong with it.
    pub rationale: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW_SAMPLE: &str = r#"{
        "confidence": 0.88,
        "rationale": "The draft opens with an abstract claim where the author always opens with a concrete example; the rest of the passage matches.",
        "mode": "review",
        "verdicts": [
            {
                "passage_excerpt": "Compression is the fundamental abstraction of writing.",
                "verdict": "propose_rewrite",
                "voice_signals": ["opens-abstract", "fundamental-overuse"],
                "proposed_rewrite": "When I cut the third draft of the lossy-compression note down from 1200 words to 400, I noticed I was doing the same thing the note describes."
            },
            {
                "passage_excerpt": "I noticed I was doing the same thing the note describes.",
                "verdict": "pass",
                "voice_signals": ["concrete-example-voice"]
            }
        ]
    }"#;

    const MODEL_UPDATE_SAMPLE: &str = r#"{
        "confidence": 0.81,
        "rationale": "The recent corpus added two distinctive patterns and dropped one. Worth surfacing for human approval; the dropped pattern is the more interesting signal.",
        "mode": "model-update",
        "model_update": {
            "additions": ["prefers-em-dash-over-colon", "double-quoted-keywords"],
            "retirements": ["explicit-numbered-lists-in-prose"],
            "rationale": "The two additions are stable across the last 30 author-written notes; the retirement is sharper — explicit lists were a 2024 habit that vanished in 2025."
        }
    }"#;

    #[test]
    fn parses_review_output() {
        let parsed: VoiceKeeperOutput =
            serde_json::from_str(REVIEW_SAMPLE).expect("review JSON must parse");
        assert_eq!(parsed.mode, VoiceKeeperMode::Review);
        assert_eq!(parsed.verdicts.len(), 2);
        assert_eq!(parsed.verdicts[0].verdict, PassageVerdict::ProposeRewrite);
        assert_eq!(parsed.verdicts[1].verdict, PassageVerdict::Pass);
        assert!(parsed.verdicts[0].proposed_rewrite.is_some());
        assert!(parsed.verdicts[1].proposed_rewrite.is_none());
        assert!(parsed.model_update.is_none());
    }

    #[test]
    fn parses_model_update_output() {
        let parsed: VoiceKeeperOutput =
            serde_json::from_str(MODEL_UPDATE_SAMPLE).expect("model-update JSON must parse");
        assert_eq!(parsed.mode, VoiceKeeperMode::ModelUpdate);
        assert!(parsed.verdicts.is_empty());
        let update = parsed.model_update.as_ref().expect("model_update present");
        assert_eq!(update.additions.len(), 2);
        assert_eq!(update.retirements.len(), 1);
    }

    #[test]
    fn round_trips_via_serde_json() {
        for sample in [REVIEW_SAMPLE, MODEL_UPDATE_SAMPLE] {
            let parsed: VoiceKeeperOutput = serde_json::from_str(sample).expect("parse");
            let re_serialized = serde_json::to_string(&parsed).expect("serialize");
            let re_parsed: VoiceKeeperOutput =
                serde_json::from_str(&re_serialized).expect("re-parse");
            assert_eq!(parsed, re_parsed, "round-trip stability");
        }
    }

    /// Every documented mode parses. If a mode is added to the
    /// prompt but the enum is not updated, the missing variant
    /// surfaces immediately.
    #[test]
    fn all_modes_parse() {
        for (json_value, expected) in [
            ("review", VoiceKeeperMode::Review),
            ("model-update", VoiceKeeperMode::ModelUpdate),
        ] {
            let parsed: VoiceKeeperMode =
                serde_json::from_str(&format!("\"{json_value}\"")).expect("mode parse");
            assert_eq!(parsed, expected, "round trip for {json_value}");
        }
    }

    /// All three verdict variants round-trip.
    #[test]
    fn all_verdicts_parse() {
        for (json_value, expected) in [
            ("pass", PassageVerdict::Pass),
            ("flag", PassageVerdict::Flag),
            ("propose_rewrite", PassageVerdict::ProposeRewrite),
        ] {
            let parsed: PassageVerdict =
                serde_json::from_str(&format!("\"{json_value}\"")).expect("verdict parse");
            assert_eq!(parsed, expected);
        }
    }

    /// Unknown modes and verdicts fail loudly — the runner's
    /// dispatch logic can't have a silent miss because the LLM
    /// hallucinated a name.
    #[test]
    fn unknown_mode_or_verdict_rejected() {
        let bad_mode = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "mode": "midnight-rave"
        }"#;
        assert!(serde_json::from_str::<VoiceKeeperOutput>(bad_mode).is_err());

        let bad_verdict = r#""acquit""#;
        assert!(serde_json::from_str::<PassageVerdict>(bad_verdict).is_err());
    }

    /// `deny_unknown_fields` is the schema-drift guardrail.
    #[test]
    fn unknown_fields_rejected() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "mode": "review",
            "future_field": "this should not parse"
        }"#;
        let err =
            serde_json::from_str::<VoiceKeeperOutput>(extra).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// ADR 0011 streaming-order contract: confidence < rationale <
    /// mode < payload. A refactor reordering struct fields would
    /// silently break the early-exit protocol.
    #[test]
    fn serializes_confidence_first() {
        let out = VoiceKeeperOutput {
            confidence: 0.9,
            rationale: "r".into(),
            mode: VoiceKeeperMode::Review,
            verdicts: vec![],
            model_update: None,
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let mode_idx = json.find("\"mode\"").expect("mode present");
        let v_idx = json.find("\"verdicts\"").expect("verdicts present");
        assert!(
            conf_idx < rat_idx && rat_idx < mode_idx && mode_idx < v_idx,
            "field order must be confidence < rationale < mode < verdicts \
             (got {conf_idx}, {rat_idx}, {mode_idx}, {v_idx})"
        );
    }
}
