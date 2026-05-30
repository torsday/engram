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

// ── Sparse-content bootstrap gate ──────────────────────────────────────────
//
// Per `docs/design/08-first-run.md` §Sparse-content bootstrap → Voice Keeper,
// the agent's behavior is *tiered* by how much human-authored material exists,
// because voice analysis needs a baseline before it can be trusted:
//
//   - `< 50` human notes  → observe-only: build the voice model passively, but
//     do NOT join council or critique agent output.
//   - `≥ 50` notes        → propose-only: join council; rewrites are always
//     reviewed, never auto-landed.
//   - `≥ 200` notes AND `≥ 30` days → mature: operate per the full design.
//
// These are the canonical thresholds. (Issue #137's original acceptance
// criteria listed `≥ 10 notes / 2K words`; that predated 08-first-run.md and is
// superseded by the tiered design above, which the rest of the bootstrap story
// — Biographer's 200/60 `SparseContentGate`, Annual Review's 12-month
// `MaturityGate` — already follows.)
//
// The snapshot type is shared with the Biographer gate; both are pure
// deterministic checks the runtime evaluates before spending LLM tokens. (A
// future slice may promote `VaultSnapshot` into a shared `sparse_content`
// module; today it lives with the Biographer gate that introduced it.)

pub use super::biographer::VaultSnapshot;

/// Which operating tier Voice Keeper is in, given the vault's size and age.
///
/// The tier is a deterministic function of a [`VaultSnapshot`] (see
/// [`VoiceKeeperBootstrap::tier`]); it gates *behavior*, not output schema —
/// the runtime reads the tier to decide whether Voice Keeper joins council and
/// whether its rewrites may ever auto-land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VoiceKeeperTier {
    /// `< 50` human notes. Build the voice model passively; do not join
    /// council or critique. The honest stance when there isn't yet enough
    /// authorial material to model a voice without fabricating one.
    ObserveOnly,
    /// `≥ 50` notes (but not yet mature). Join council; every rewrite is
    /// reviewed, never auto-landed.
    ProposeOnly,
    /// `≥ 200` notes and `≥ 30` days. Operate per the full mature design.
    Mature,
}

impl VoiceKeeperTier {
    /// Whether Voice Keeper participates in council deliberations in this
    /// tier. False only in [`ObserveOnly`](VoiceKeeperTier::ObserveOnly).
    pub fn participates_in_council(self) -> bool {
        !matches!(self, Self::ObserveOnly)
    }

    /// Whether a Voice Keeper rewrite may ever auto-land in this tier. Only
    /// the mature tier may auto-land; below it, every rewrite is reviewed.
    pub fn may_auto_land(self) -> bool {
        matches!(self, Self::Mature)
    }
}

/// Deterministic gate that classifies Voice Keeper's operating tier from a
/// vault snapshot. Thresholds default to the design-doc values; they are
/// fields so a test (or a future per-user override) can vary them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceKeeperBootstrap {
    /// Below this human-note count, Voice Keeper is observe-only.
    pub observe_below_notes: u32,
    /// At or above `observe_below_notes` but below this, Voice Keeper is
    /// propose-only. At or above this (with sufficient age) it is mature.
    pub mature_min_notes: u32,
    /// Minimum vault age in days for the mature tier (in addition to
    /// `mature_min_notes`).
    pub mature_min_age_days: u32,
}

impl Default for VoiceKeeperBootstrap {
    /// Design-doc thresholds: observe-only `< 50`, propose-only `≥ 50`,
    /// mature `≥ 200` notes and `≥ 30` days.
    fn default() -> Self {
        Self {
            observe_below_notes: 50,
            mature_min_notes: 200,
            mature_min_age_days: 30,
        }
    }
}

impl VoiceKeeperBootstrap {
    /// Classify the operating tier for a given vault snapshot.
    ///
    /// Mature requires BOTH enough notes and enough age — a 300-note vault
    /// that is only a week old is still propose-only, because a voice model
    /// built from a single intense week may not generalize.
    pub fn tier(&self, snapshot: VaultSnapshot) -> VoiceKeeperTier {
        if snapshot.human_notes_total < self.observe_below_notes {
            VoiceKeeperTier::ObserveOnly
        } else if snapshot.human_notes_total >= self.mature_min_notes
            && snapshot.age_days >= self.mature_min_age_days
        {
            VoiceKeeperTier::Mature
        } else {
            VoiceKeeperTier::ProposeOnly
        }
    }

    /// Human-readable reason string for the observe-only tier, suitable for the
    /// standup line "Voice Keeper sleeping: …". Returns `None` when the tier is
    /// not observe-only (nothing to explain — the agent is active).
    pub fn observe_only_reason(&self, snapshot: VaultSnapshot) -> Option<String> {
        match self.tier(snapshot) {
            VoiceKeeperTier::ObserveOnly => Some(format!(
                "Voice Keeper is observe-only: {} human notes ({} required to join \
                 council). Building the voice model passively until then.",
                snapshot.human_notes_total, self.observe_below_notes,
            )),
            VoiceKeeperTier::ProposeOnly | VoiceKeeperTier::Mature => None,
        }
    }
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

    // ── Sparse-content bootstrap gate ──────────────────────────────────────

    fn snap(notes: u32, age_days: u32) -> VaultSnapshot {
        VaultSnapshot {
            human_notes_total: notes,
            age_days,
        }
    }

    #[test]
    fn tier_below_50_notes_is_observe_only() {
        let g = VoiceKeeperBootstrap::default();
        assert_eq!(g.tier(snap(0, 0)), VoiceKeeperTier::ObserveOnly);
        assert_eq!(g.tier(snap(49, 365)), VoiceKeeperTier::ObserveOnly);
        // Age is irrelevant below the note floor.
        assert!(!g.tier(snap(49, 365)).participates_in_council());
    }

    #[test]
    fn tier_50_to_199_notes_is_propose_only() {
        let g = VoiceKeeperBootstrap::default();
        assert_eq!(g.tier(snap(50, 30)), VoiceKeeperTier::ProposeOnly);
        assert_eq!(g.tier(snap(199, 9_999)), VoiceKeeperTier::ProposeOnly);
        let t = g.tier(snap(50, 30));
        assert!(t.participates_in_council(), "propose-only joins council");
        assert!(!t.may_auto_land(), "propose-only never auto-lands");
    }

    #[test]
    fn tier_mature_requires_both_notes_and_age() {
        let g = VoiceKeeperBootstrap::default();
        // Enough notes but too young → still propose-only.
        assert_eq!(g.tier(snap(300, 29)), VoiceKeeperTier::ProposeOnly);
        // Both thresholds met → mature.
        assert_eq!(g.tier(snap(200, 30)), VoiceKeeperTier::Mature);
        assert!(g.tier(snap(200, 30)).may_auto_land());
    }

    #[test]
    fn observe_only_reason_present_only_when_observe_only() {
        let g = VoiceKeeperBootstrap::default();
        let r = g
            .observe_only_reason(snap(12, 5))
            .expect("observe-only reason");
        assert!(
            r.contains("12") && r.contains("50"),
            "reason names the figures: {r}"
        );
        assert!(g.observe_only_reason(snap(50, 30)).is_none());
        assert!(g.observe_only_reason(snap(200, 30)).is_none());
    }

    #[test]
    fn tier_round_trips_via_serde_kebab_case() {
        for (t, s) in [
            (VoiceKeeperTier::ObserveOnly, "\"observe-only\""),
            (VoiceKeeperTier::ProposeOnly, "\"propose-only\""),
            (VoiceKeeperTier::Mature, "\"mature\""),
        ] {
            assert_eq!(serde_json::to_string(&t).unwrap(), s);
            assert_eq!(
                serde_json::from_str::<VoiceKeeperTier>(s).unwrap(),
                t,
                "kebab-case round trip for {s}"
            );
        }
    }
}
