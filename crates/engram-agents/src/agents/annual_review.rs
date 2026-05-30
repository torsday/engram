//! Annual Review agent — produces a yearly long-form narrative reflection on
//! the vault's evolution.
//!
//! Per `docs/design/01-agents-and-council.md` §Annual Review the agent's job is,
//! once per year, to read everything (git log, major evergreens, deliberation
//! history, Historian digests, concept-trajectory output) and write a long-form
//! reflection to `reflections/annual/YYYY.md` — themes, evolution, what
//! crystallized vs. what was abandoned, key insights, intellectual milestones.
//! It is "probably the most emotionally resonant artifact the system produces"
//! and **always** goes through human approval (the user curates their own
//! reflection).
//!
//! # What this module contributes
//!
//! The host runtime ([`crate::runner::AgentRunner`]) is agent-agnostic. This
//! module supplies the Annual-Review-specific bits the runtime does **not**
//! know:
//!
//! - [`AnnualReviewOutput`] — the structured-JSON type the LLM must produce.
//!   Field order matches the streaming early-exit invariant from
//!   [#23](https://github.com/torsday/engram/issues/23): `confidence` first,
//!   `rationale` second, payload after — so a streaming consumer can abort
//!   before paying for the (large) `narrative` payload when confidence is low.
//! - [`MaturityGate`] — pure deterministic check. The Annual Review must
//!   abstain until the vault is at least twelve months old (the first review
//!   covers months 1–12). This gate is checked **before** spending tokens; if
//!   it trips, the runtime can short-circuit to a
//!   [`AnnualReviewOutput::maturity_stub`].
//! - [`compute_confidence`] — combines the LLM self-score with deterministic
//!   signals (corpus coverage and temporal span) per ADR 0013's
//!   "deterministic where possible" principle. The formula is documented at
//!   the function.
//! - [`TOOLS`] — the tool names Annual Review declares it needs. The tool
//!   gateway slice validates these at startup once it lands; the constant lives
//!   here so consumers see one authoritative list.
//!
//! # What this module deliberately does NOT do
//!
//! - **Schedule wiring.** Triggered by `cron` with a yearly period — the
//!   scheduler picks this up from `config.toml` once the agent is registered.
//! - **Output-path selection / proposal filing.** Annual Review always goes
//!   through the proposal path (it's a personal artifact; humans curate). The
//!   decision-matrix slice reads `[annual-review].always_propose = true` and
//!   routes accordingly. This module produces the structured output; the
//!   runtime files the proposal.
//! - **Tool implementations.** Hybrid retrieval, git-log summarisation,
//!   longitudinal data assembly, Voice Keeper model access — these live in
//!   other crates. Annual Review just names what it needs.

use serde::{Deserialize, Serialize};

// ── Tools the Annual Review agent declares it needs ─────────────────────────

/// Tool names Annual Review declares in its config. Order matches `config.toml`
/// for sanity, but the gateway treats the list as a set.
///
/// These are *declarative*: the runner's tool-gateway slice validates that each
/// named tool is bound in the registry at startup. Tool implementations live in
/// other crates (e.g. `engram-index::fts::hybrid_search`,
/// `engram-git::log_summary`).
pub const TOOLS: &[&str] = &[
    "hybrid_search",
    "git_log_summary",
    "activity_log_reader",
    "deliberation_history",
    "concept_trajectory",
    "voice_keeper_model",
];

// ── Structured output ──────────────────────────────────────────────────────

/// JSON payload the Annual Review LLM must return.
///
/// Field order in the serialized form is fixed: `confidence` → `rationale` →
/// `maturity_gate` → `year` → `output_path` → `themes` → `milestones` →
/// `narrative`. The first-two-fields invariant is required for streaming
/// early-exit per [#23](https://github.com/torsday/engram/issues/23): a
/// streaming consumer can terminate the call once `confidence` is below
/// threshold, before paying for the heavy `narrative` tokens.
///
/// The `maturity_gate` field is set by the *LLM* (the prompt asks it to honour
/// [`MaturityGate`]). The host *also* enforces the gate deterministically
/// before invoking the agent — the two are belt-and-braces.
///
/// `deny_unknown_fields` matches the house convention for typed agent outputs
/// (see siblings in [`super`]): strict validation via [`super::validate`]
/// rejects responses carrying fields outside the documented schema. The
/// runner's hot path stays permissive (`parse_confidence`), so a schema drift
/// surfaces in eval / CI rather than taking down the gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnualReviewOutput {
    /// LLM self-assessed confidence in the reflection as a whole, in
    /// `[0.0, 1.0]`. Combined with deterministic signals via
    /// [`compute_confidence`] before being recorded.
    pub confidence: f32,

    /// One paragraph: what signals shaped this reflection (which themes
    /// recurred, what the git log and activity log surfaced) and what could be
    /// incomplete. Watcher reads this to track calibration over time.
    pub rationale: String,

    /// `true` when the vault was too young to write a real annual review —
    /// `narrative` is then empty and `themes`/`milestones` are empty. Mirrors
    /// the host's [`MaturityGate`] verdict; they should always agree.
    #[serde(default)]
    pub maturity_gate: bool,

    /// The calendar year the review covers (e.g. `2026`). On the first review,
    /// this is the year spanning the vault's first twelve months.
    pub year: i32,

    /// Vault-relative path where the reflection will be written, e.g.
    /// `"reflections/annual/2026.md"`. Always under `reflections/annual/`.
    /// Empty when [`maturity_gate`](Self::maturity_gate) is `true` (nothing is
    /// written on abstention).
    pub output_path: String,

    /// Key themes surfaced across the year, in the user's own conceptual
    /// vocabulary. Empty on abstention. Distinct from `milestones`: themes are
    /// the *recurring motifs*, milestones are the *discrete events*.
    #[serde(default)]
    pub themes: Vec<String>,

    /// Intellectual milestones reached during the year — ideas that
    /// crystallized, threads that were abandoned, positions that shifted.
    /// Empty on abstention.
    #[serde(default)]
    pub milestones: Vec<String>,

    /// The full markdown content of `reflections/annual/YYYY.md` — the
    /// long-form narrative reflection written in the user's voice. Empty on
    /// abstention. This is the heaviest field, intentionally serialized last
    /// so streaming early-exit can abort before it.
    pub narrative: String,
}

impl AnnualReviewOutput {
    /// Convenience constructor for the maturity-gate abstention stub. Used by
    /// callers short-circuiting on [`MaturityGate::should_abstain`]; the LLM
    /// would produce something shaped identically, so callers can also
    /// construct this without invoking the model and file a proposal indicating
    /// "the vault isn't a year old yet — no annual review to write."
    ///
    /// `rationale` is supplied by the caller so the stub can carry the gate's
    /// specific reason (e.g. "vault is 200 days old; 365 required"). `year` is
    /// the year that *would* be reviewed once the vault matures.
    pub fn maturity_stub(year: i32, rationale: impl Into<String>) -> Self {
        Self {
            // The maturity stub is a *deterministic* assertion — high
            // confidence in the abstention itself, not in any reflective
            // content. Calibration treats this case specially.
            confidence: 1.0,
            rationale: rationale.into(),
            maturity_gate: true,
            year,
            // Nothing is written on abstention, so there is no output path.
            output_path: String::new(),
            themes: Vec::new(),
            milestones: Vec::new(),
            narrative: String::new(),
        }
    }
}

// ── Maturity gate ──────────────────────────────────────────────────────────

/// Deterministic abstention check: the Annual Review will not run until the
/// vault is at least twelve months old. A vault younger than a full year has
/// no "year" to reflect on; the first review covers months 1–12.
///
/// Per `docs/design/01-agents-and-council.md` §Annual Review and the issue #61
/// acceptance criterion ("doesn't run until vault is ≥ 12 months old"), the
/// threshold is 365 days. The host enforces this *before* invoking the LLM —
/// saves the expensive `deep`-tier tokens on a guaranteed-stub outcome.
///
/// Treat this as a value type: it's just a threshold. Use
/// [`MaturityGate::default`] for the design-doc threshold, or construct
/// directly with a custom value (used by tests with a smaller floor).
#[derive(Debug, Clone, PartialEq)]
pub struct MaturityGate {
    /// Minimum age of the vault in days (days since first note) before an
    /// annual review may run.
    pub min_vault_age_days: u32,
}

impl Default for MaturityGate {
    /// Default matches `agents/annual-review/config.toml` and the design doc:
    /// 365 days (twelve months).
    fn default() -> Self {
        Self {
            min_vault_age_days: 365,
        }
    }
}

impl MaturityGate {
    /// Decide whether to abstain.
    ///
    /// Returns `None` when the vault is old enough — the agent should run
    /// normally. Returns `Some(reason)` when it is not, with a short
    /// human-readable explanation suitable for the proposal's rationale field.
    /// The `Some` variant is the "abstain" verdict.
    ///
    /// The intent is `if let Some(reason) = gate.should_abstain(age_days)` —
    /// the natural read is "if there's a reason to abstain, abstain."
    pub fn should_abstain(&self, age_days: u32) -> Option<String> {
        if age_days >= self.min_vault_age_days {
            None
        } else {
            Some(format!(
                "Vault too young for an annual review: {} days since first note \
                 ({} required). The Annual Review abstains until the vault has a \
                 full year to reflect on; the first review will cover months 1–12.",
                age_days, self.min_vault_age_days,
            ))
        }
    }
}

// ── Confidence formula ─────────────────────────────────────────────────────

/// Deterministic signals that combine with the LLM's self-score to produce the
/// recorded confidence.
///
/// All fields are normalized to `[0.0, 1.0]`. They are produced upstream by
/// pure functions over the year's vault snapshot — they are not LLM output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceSignals {
    /// LLM's self-reported `confidence` from [`AnnualReviewOutput`].
    pub llm_self_score: f32,
    /// Coverage signal: how much of the year's material did the agent's
    /// retrieval actually read? `notes_read / notes_in_year`. A reflection
    /// synthesised from 5% of the year's notes should be penalised vs one that
    /// covered half.
    pub corpus_coverage: f32,
    /// Temporal-span signal: across how much of the twelve-month window was
    /// there note activity? `months_with_activity / 12`. A year with writing
    /// in only two months yields a thinner, less-defensible reflection than a
    /// year of sustained activity.
    pub temporal_span: f32,
}

/// Combine the LLM self-score with deterministic vault signals.
///
/// **Formula** (chosen to match the reference confidence formula shape in
/// `docs/design/12-agent-spec-template.md` §Linker §Confidence formula and the
/// sibling Biographer agent, adapted to Annual Review's signals):
///
/// ```text
/// final = 0.5 × llm_self_score
///       + 0.3 × corpus_coverage
///       + 0.2 × temporal_span
/// ```
///
/// Weights:
///
/// - **0.5 on LLM self-score** — the model has the strongest view of its own
///   uncertainty when it's well-calibrated.
/// - **0.3 on corpus coverage** — a reflection grounded in most of the year's
///   notes is durably trustworthy; one that skimmed a slice is not.
/// - **0.2 on temporal span** — a low ceiling on confidence when the year had
///   activity in only a few months (less to reflect on, more risk of
///   over-reading sparse signal).
///
/// Any value outside `[0.0, 1.0]` in the inputs is clamped before computation.
/// The output is always in `[0.0, 1.0]`.
///
/// **Maturity-gate special case:** when the agent has been short-circuited to
/// the stub via [`MaturityGate::should_abstain`], do not call this function —
/// the stub uses `confidence = 1.0` (high confidence in the *abstention*, not
/// in any reflective content; see [`AnnualReviewOutput::maturity_stub`]).
pub fn compute_confidence(signals: ConfidenceSignals) -> f32 {
    let self_score = signals.llm_self_score.clamp(0.0, 1.0);
    let coverage = signals.corpus_coverage.clamp(0.0, 1.0);
    let span = signals.temporal_span.clamp(0.0, 1.0);
    (0.5 * self_score + 0.3 * coverage + 0.2 * span).clamp(0.0, 1.0)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── AnnualReviewOutput shape ────────────────────────────────────────

    /// The first-two-fields invariant (`confidence` then `rationale`) is
    /// load-bearing for streaming early-exit per #23. A regression that
    /// reorders fields would silently break that path. Pin the order via the
    /// serialized JSON, and confirm the heavy `narrative` field serializes
    /// last.
    #[test]
    fn output_field_order_is_streaming_compatible() {
        let out = AnnualReviewOutput {
            confidence: 0.9,
            rationale: "well-grounded".into(),
            maturity_gate: false,
            year: 2026,
            output_path: "reflections/annual/2026.md".into(),
            themes: vec!["attention".into()],
            milestones: vec!["shipped engram".into()],
            narrative: "## 2026\n\nA year of building.".into(),
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let confidence_pos = json.find("\"confidence\"").expect("has confidence");
        let rationale_pos = json.find("\"rationale\"").expect("has rationale");
        let narrative_pos = json.find("\"narrative\"").expect("has narrative");
        assert_eq!(
            confidence_pos, 1,
            "confidence must be first key, got json: {json}"
        );
        assert!(
            confidence_pos < rationale_pos,
            "confidence must precede rationale"
        );
        assert!(
            rationale_pos < narrative_pos,
            "the heavy narrative payload must serialize last for streaming early-exit"
        );
    }

    /// Round-trip the LLM's expected output shape. Catches accidental breakage
    /// of the `Deserialize` impl (renamed field, changed type).
    #[test]
    fn output_deserializes_from_canonical_json() {
        let json = r###"{
            "confidence": 0.88,
            "rationale": "Three themes recurred across 14 of 18 active months; drift is clear.",
            "maturity_gate": false,
            "year": 2026,
            "output_path": "reflections/annual/2026.md",
            "themes": ["legibility", "attention", "tooling"],
            "milestones": ["shipped engram v1", "abandoned the chat UI thread"],
            "narrative": "## 2026\n\nThis was the year the vault learned to rewrite itself."
        }"###;
        let out: AnnualReviewOutput = serde_json::from_str(json).expect("parse");
        assert!((out.confidence - 0.88).abs() < 1e-6);
        assert!(out.rationale.contains("recurred"));
        assert!(!out.maturity_gate);
        assert_eq!(out.year, 2026);
        assert_eq!(out.output_path, "reflections/annual/2026.md");
        assert_eq!(out.themes.len(), 3);
        assert_eq!(out.milestones.len(), 2);
        assert!(out.narrative.contains("rewrite itself"));
    }

    /// `themes` and `milestones` default to empty when omitted — a minimal
    /// output (the smallest valid body) need not carry them.
    #[test]
    fn themes_and_milestones_default_empty() {
        let minimal = r#"{
            "confidence": 0.7,
            "rationale": "r",
            "year": 2025,
            "output_path": "reflections/annual/2025.md",
            "narrative": "x"
        }"#;
        let out: AnnualReviewOutput = serde_json::from_str(minimal).expect("parse");
        assert!(out.themes.is_empty());
        assert!(out.milestones.is_empty());
        assert!(!out.maturity_gate, "maturity_gate defaults to false");
    }

    /// Required fields really are required — a body missing `narrative` fails.
    #[test]
    fn narrative_is_required() {
        let no_narrative = r#"{
            "confidence": 0.7,
            "rationale": "r",
            "year": 2025,
            "output_path": "reflections/annual/2025.md"
        }"#;
        assert!(
            serde_json::from_str::<AnnualReviewOutput>(no_narrative).is_err(),
            "missing narrative must fail"
        );
    }

    /// Schema discipline: per the house convention (`deny_unknown_fields`), an
    /// output carrying a field outside the documented schema is *rejected*, not
    /// silently accepted. This is what `super::validate` relies on to catch
    /// prompt/struct drift in eval and CI.
    #[test]
    fn unknown_fields_in_output_are_rejected() {
        let json = r#"{
            "confidence": 0.5,
            "rationale": "x",
            "year": 2026,
            "output_path": "reflections/annual/2026.md",
            "narrative": "n",
            "future_field": "should be rejected"
        }"#;
        let err = serde_json::from_str::<AnnualReviewOutput>(json)
            .expect_err("deny_unknown_fields must reject extra keys");
        assert!(
            err.to_string().contains("future_field"),
            "error should name the offending field, got: {err}"
        );
    }

    // ── maturity_stub ───────────────────────────────────────────────────

    /// The stub is what the host produces when the gate trips without spending
    /// tokens. Verify its invariants: the gate flag is `true`, narrative and
    /// lists are empty, no output path, the rationale carries the reason, and
    /// confidence is 1.0 (high confidence in the *abstention*).
    #[test]
    fn maturity_stub_carries_gate_flag_and_empty_payload() {
        let stub = AnnualReviewOutput::maturity_stub(2026, "vault is 200 days old; need 365");
        assert!(stub.maturity_gate, "stub must mark the gate tripped");
        assert_eq!(stub.year, 2026);
        assert!(
            stub.rationale.contains("200"),
            "rationale must carry the specific reason"
        );
        assert!(
            stub.output_path.is_empty(),
            "nothing is written on abstention"
        );
        assert!(stub.narrative.is_empty(), "narrative must be empty");
        assert!(stub.themes.is_empty(), "themes must be empty");
        assert!(stub.milestones.is_empty(), "milestones must be empty");
        assert!((stub.confidence - 1.0).abs() < f32::EPSILON);
    }

    /// The stub round-trips through serde — the runner serializes it as a
    /// response and parses it back.
    #[test]
    fn maturity_stub_round_trips() {
        let stub = AnnualReviewOutput::maturity_stub(2026, "too young");
        let json = serde_json::to_string(&stub).expect("serialize");
        let back: AnnualReviewOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stub, back);
    }

    // ── MaturityGate logic ──────────────────────────────────────────────

    /// Default threshold matches the design doc (365 days). A silent change
    /// would alter the agent's gate behaviour; pin the value.
    #[test]
    fn default_threshold_matches_design_doc() {
        assert_eq!(MaturityGate::default().min_vault_age_days, 365);
    }

    /// At-threshold passes (not strictly less than). The issue says
    /// "≥ 12 months" — boundary case at exactly 365 days.
    #[test]
    fn gate_passes_exactly_at_threshold() {
        assert!(MaturityGate::default().should_abstain(365).is_none());
    }

    /// One day short is an abstention. Catches off-by-one in the comparison,
    /// and the reason must carry the actual figures for the proposal rationale.
    #[test]
    fn gate_abstains_one_day_short() {
        let r = MaturityGate::default().should_abstain(364);
        assert!(r.is_some(), "one day short must abstain, got: {r:?}");
        let reason = r.unwrap();
        assert!(reason.contains("364"));
        assert!(reason.contains("365"));
    }

    /// A brand-new vault abstains.
    #[test]
    fn gate_abstains_for_new_vault() {
        assert!(MaturityGate::default().should_abstain(0).is_some());
    }

    /// Custom thresholds (e.g. a test harness wanting a smaller floor) override
    /// the default cleanly.
    #[test]
    fn custom_threshold_is_honoured() {
        let g = MaturityGate {
            min_vault_age_days: 30,
        };
        assert!(g.should_abstain(30).is_none());
        assert!(g.should_abstain(29).is_some());
    }

    // ── compute_confidence ──────────────────────────────────────────────

    /// Pinned formula output for a representative input. Catches silent
    /// coefficient drift — if anyone changes the weights, this test surfaces it
    /// immediately.
    #[test]
    fn formula_pins_canonical_weights() {
        let c = compute_confidence(ConfidenceSignals {
            llm_self_score: 0.8,
            corpus_coverage: 0.6,
            temporal_span: 0.5,
        });
        // 0.5*0.8 + 0.3*0.6 + 0.2*0.5 = 0.40 + 0.18 + 0.10 = 0.68
        assert!(
            (c - 0.68).abs() < 1e-5,
            "formula drifted: expected 0.68, got {c}"
        );
    }

    /// Clamping floor at 0.0 — even all-negative signals stay non-negative.
    #[test]
    fn formula_clamps_at_zero_floor() {
        let c = compute_confidence(ConfidenceSignals {
            llm_self_score: -1.0,
            corpus_coverage: -1.0,
            temporal_span: -1.0,
        });
        assert_eq!(c, 0.0);
    }

    /// Clamping ceiling at 1.0 — over-the-top inputs do not exceed the declared
    /// range. Downstream consumers (Watcher, scorecards) rely on `[0.0, 1.0]`.
    #[test]
    fn formula_clamps_at_one_ceiling() {
        let c = compute_confidence(ConfidenceSignals {
            llm_self_score: 2.0,
            corpus_coverage: 2.0,
            temporal_span: 2.0,
        });
        assert_eq!(c, 1.0);
    }

    /// Monotonicity in the LLM-self-score axis. Holding deterministic signals
    /// fixed, a higher self-score must yield ≥ confidence. Guards the
    /// "0.5 weight on self-score" assumption against a future sign flip.
    #[test]
    fn formula_is_monotonic_in_self_score() {
        let mk = |s| ConfidenceSignals {
            llm_self_score: s,
            corpus_coverage: 0.5,
            temporal_span: 0.5,
        };
        let low = compute_confidence(mk(0.1));
        let mid = compute_confidence(mk(0.5));
        let high = compute_confidence(mk(0.9));
        assert!(low <= mid, "low={low} mid={mid}");
        assert!(mid <= high, "mid={mid} high={high}");
    }
}
