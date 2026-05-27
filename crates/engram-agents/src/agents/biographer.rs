//! Biographer agent — maintains `meta/biography.md`, the system's evolving
//! model of who the user is.
//!
//! Per `docs/design/01-agents-and-council.md` §Biographer the agent's job is
//! to update a single note with six sections (Identity, Domains of expertise,
//! Recurring themes, Stated commitments, Open questions, Drift since last
//! update) on a monthly schedule, based on the vault's drift over time.
//! Other agents inject this note into their context to ground their work
//! in a coherent picture of the user.
//!
//! # What this module contributes
//!
//! The host runtime ([`crate::runner::AgentRunner`]) is agent-agnostic. This
//! module supplies the Biographer-specific bits the runtime does **not**
//! know:
//!
//! - [`BiographerOutput`] — the structured-JSON type the LLM must produce.
//!   Field order matches the streaming early-exit invariant from
//!   [#23](https://github.com/torsday/engram/issues/23): `confidence` first,
//!   `rationale` second, payload after.
//! - [`SparseContentGate`] — pure deterministic check. The Biographer must
//!   abstain when the vault is too small (per the sparse-content bootstrap
//!   in `docs/design/08-first-run.md`). This gate is checked **before**
//!   spending tokens; if it trips, the runtime can short-circuit to an
//!   empty-stub proposal.
//! - [`compute_confidence`] — combines the LLM self-score with deterministic
//!   signals (corpus coverage and within-section corroboration) per ADR
//!   0013's "deterministic where possible" principle. The formula is
//!   documented at the function.
//! - [`TOOLS`] — the tool names Biographer declares it needs. The tool
//!   gateway slice (placeholder in [`crate::lib.rs`]) will validate these
//!   at startup once it lands; the constant lives here so consumers see one
//!   authoritative list.
//!
//! # What this module deliberately does NOT do
//!
//! - **Schedule wiring.** Triggered by `cron` with a 30-day period — the
//!   scheduler picks this up from `config.toml` once Biographer is registered.
//! - **Output-path selection.** Biographer always goes through the proposal
//!   path (it models the user; humans curate themselves). The decision-matrix
//!   slice (follow-up to #27) reads `[biographer].always_propose = true` and
//!   routes accordingly. This module produces the structured output; the
//!   runtime files the proposal.
//! - **Tool implementations.** Topic clustering, frontmatter querying, git
//!   log summarization, provenance filtering — these live in other crates.
//!   Biographer just names what it needs.

use serde::{Deserialize, Serialize};

// ── Tools the Biographer declares it needs ─────────────────────────────────

/// Tool names Biographer declares in its config. Order matches `config.toml`
/// for sanity, but the gateway treats the list as a set.
///
/// These are *declarative*: the runner's tool-gateway slice (lib.rs
/// placeholder, to be implemented) validates that each named tool is bound
/// in the registry at startup. Tool implementations live in other crates
/// (e.g. `engram-index::fts::hybrid_search`, `engram-git::log_summary`).
pub const TOOLS: &[&str] = &[
    "hybrid_search",
    "topic_cluster",
    "frontmatter_query",
    "git_log_summary",
    "provenance_filter",
];

// ── Structured output ──────────────────────────────────────────────────────

/// JSON payload the Biographer LLM must return.
///
/// Field order in the serialized form is fixed: `confidence` → `rationale`
/// → `sparse_content_gate` → `sections`. The first-two-fields invariant is
/// required for streaming early-exit per
/// [#23](https://github.com/torsday/engram/issues/23): a streaming consumer
/// can decide to terminate the call once `confidence` is below threshold,
/// before paying for `sections` tokens.
///
/// The `sparse_content_gate` field is set by the *LLM* (the prompt asks it
/// to honour [`SparseContentGate`]). The host *also* enforces the gate
/// deterministically before invoking the agent — the two are belt-and-braces.
///
/// `deny_unknown_fields` matches the house convention for typed agent
/// outputs (see siblings in [`super`]): strict validation via
/// [`super::validate`] rejects responses carrying fields outside the
/// documented schema. The runner's hot path stays permissive
/// (`parse_confidence`), so a schema drift surfaces in eval / CI rather
/// than taking down the gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BiographerOutput {
    /// LLM self-assessed confidence in the biography as a whole, in
    /// `[0.0, 1.0]`. Combined with deterministic signals via
    /// [`compute_confidence`] before being recorded.
    pub confidence: f32,
    /// One-paragraph self-explanation: what signals shaped this update and
    /// what could be wrong. Watcher reads this to track calibration over
    /// time.
    pub rationale: String,
    /// `true` when the vault was too sparse to write a real biography —
    /// every section in `sections` is then empty. Mirrors the host's
    /// [`SparseContentGate`] verdict; they should always agree.
    #[serde(default)]
    pub sparse_content_gate: bool,
    /// The six markdown sections that compose `meta/biography.md`.
    pub sections: BiographySections,
}

/// The six sections of `meta/biography.md`. Each field is rendered as
/// markdown under a same-named `##` heading. Order matches the design
/// doc's stipulated output structure.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BiographySections {
    /// `## Identity` — 1–3 paragraphs, the user distilled.
    pub identity: String,
    /// `## Domains of expertise` — bulleted list of domains the vault
    /// repeatedly demonstrates depth in.
    pub domains_of_expertise: String,
    /// `## Recurring themes` — bulleted list of motifs that surface across
    /// many notes.
    pub recurring_themes: String,
    /// `## Stated commitments` — bulleted list of explicit positions the
    /// user has written.
    pub stated_commitments: String,
    /// `## Open questions` — bulleted list of questions the vault keeps
    /// returning to without resolving.
    pub open_questions: String,
    /// `## Drift since last update` — paragraph: what changed since the
    /// previous biography.
    pub drift_since_last_update: String,
}

impl BiographerOutput {
    /// Convenience constructor for the sparse-content stub. Used by callers
    /// short-circuiting on [`SparseContentGate::should_abstain`]; the LLM
    /// would produce something shaped identically, so callers can also
    /// construct this without invoking the model and file a proposal
    /// indicating "not enough vault to write a biography yet".
    ///
    /// `rationale` is supplied by the caller so the stub can carry the
    /// gate's specific reason (e.g. "85 human notes; threshold is 200").
    pub fn sparse_stub(rationale: impl Into<String>) -> Self {
        Self {
            // Sparse-content stub is a *deterministic* assertion — high
            // confidence in the abstention itself, not in any biographical
            // content. Calibration treats this case specially.
            confidence: 1.0,
            rationale: rationale.into(),
            sparse_content_gate: true,
            sections: BiographySections::default(),
        }
    }
}

// ── Sparse-content gate ────────────────────────────────────────────────────

/// Deterministic abstention check: when the vault is too small the
/// Biographer returns an empty stub rather than fabricating a user model.
///
/// Per `docs/design/08-first-run.md` §Sparse-content bootstrap, the
/// thresholds are 200 human-authored notes spanning 60 days. **Both** must
/// be met before the agent will produce a real biography. The host enforces
/// this *before* invoking the LLM — saves tokens on a guaranteed-stub
/// outcome.
///
/// Treat this as a value type: it's just two thresholds plus a snapshot.
/// Use [`SparseContentGate::default`] for the design-doc thresholds, or
/// construct directly with custom values (used by tests with a smaller
/// floor). A `from_config(&AgentConfig)` constructor reading the
/// `[biographer]` TOML table lands in the same follow-up slice that
/// extends `AgentConfig` to expose per-agent custom sections.
#[derive(Debug, Clone, PartialEq)]
pub struct SparseContentGate {
    /// Minimum number of human-authored notes in the vault.
    pub min_human_notes: u32,
    /// Minimum age of the vault in days (days since first note).
    pub min_vault_age_days: u32,
}

/// Live vault statistics the gate evaluates against. Producing this is the
/// caller's job (a deterministic query against the SQLite index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultSnapshot {
    /// Total human-authored notes (provenance filtered).
    pub human_notes_total: u32,
    /// Days since the first note's `created_at`.
    pub age_days: u32,
}

impl Default for SparseContentGate {
    /// Defaults match `agents/biographer/config.toml` and the design doc:
    /// 200 notes, 60 days.
    fn default() -> Self {
        Self {
            min_human_notes: 200,
            min_vault_age_days: 60,
        }
    }
}

impl SparseContentGate {
    /// Decide whether to abstain.
    ///
    /// Returns `None` when the vault is large enough — the agent should run
    /// normally. Returns `Some(reason)` when it is not, with a short
    /// human-readable explanation suitable for the proposal's rationale
    /// field. The `Some` variant is the "abstain" verdict.
    ///
    /// The intent is `if let Some(reason) = gate.should_abstain(...)` —
    /// the natural read is "if there's a reason to abstain, abstain."
    pub fn should_abstain(&self, snapshot: VaultSnapshot) -> Option<String> {
        let notes_short = snapshot.human_notes_total < self.min_human_notes;
        let age_short = snapshot.age_days < self.min_vault_age_days;
        match (notes_short, age_short) {
            (false, false) => None,
            (true, true) => Some(format!(
                "Vault too sparse: {} human notes ({} required) over {} days ({} required). \
                 Biographer abstains until the vault is mature enough to model.",
                snapshot.human_notes_total,
                self.min_human_notes,
                snapshot.age_days,
                self.min_vault_age_days,
            )),
            (true, false) => Some(format!(
                "Vault too sparse: {} human notes ({} required). \
                 Biographer abstains until enough material exists.",
                snapshot.human_notes_total, self.min_human_notes,
            )),
            (false, true) => Some(format!(
                "Vault too young: {} days since first note ({} required). \
                 Biographer abstains until the vault has matured for at least \
                 {} days, even if the note count is sufficient.",
                snapshot.age_days, self.min_vault_age_days, self.min_vault_age_days,
            )),
        }
    }
}

// ── Confidence formula ─────────────────────────────────────────────────────

/// Deterministic signals that combine with the LLM's self-score to produce
/// the recorded confidence.
///
/// All fields are normalized to `[0.0, 1.0]`. They are produced upstream by
/// pure functions over the vault snapshot — they are not LLM output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceSignals {
    /// LLM's self-reported `confidence` from [`BiographerOutput`].
    pub llm_self_score: f32,
    /// Coverage signal: how much of the vault did the agent's retrieval
    /// actually see? `notes_seen / human_notes_total`. A biography written
    /// after sampling 5% of the corpus should be penalized vs one that
    /// covered 50%.
    pub corpus_coverage: f32,
    /// Within-section corroboration: per claim, how many distinct notes
    /// support it? The caller averages a per-claim score across all
    /// non-empty sections. `0.0` if no claim has > 1 supporting note;
    /// `1.0` if every claim has ≥ 5 supporting notes.
    pub corroboration: f32,
}

/// Combine LLM self-score with deterministic vault signals.
///
/// **Formula** (chosen to match Linker's reference confidence formula in
/// shape — see `docs/design/12-agent-spec-template.md` §Linker §Confidence
/// formula — adapted to Biographer's signals):
///
/// ```text
/// final = 0.5 × llm_self_score
///       + 0.3 × corroboration
///       + 0.2 × corpus_coverage
/// ```
///
/// Weights:
///
/// - **0.5 on LLM self-score** — the model has the strongest view of its
///   own uncertainty when it's well-calibrated.
/// - **0.3 on corroboration** — biographies grounded in many notes per
///   claim are durably trustworthy regardless of LLM mood.
/// - **0.2 on corpus coverage** — a low ceiling on confidence when the
///   agent only saw a slice of the vault.
///
/// Any value outside `[0.0, 1.0]` in the inputs is clamped before
/// computation. The output is always in `[0.0, 1.0]`.
///
/// **Sparse-content special case:** when the agent has been short-circuited
/// to the empty stub via [`SparseContentGate::should_abstain`], do not call
/// this function — the stub uses `confidence = 1.0` (high confidence in
/// the *abstention*, not in any biographical content; see
/// [`BiographerOutput::sparse_stub`]).
pub fn compute_confidence(signals: ConfidenceSignals) -> f32 {
    let self_score = signals.llm_self_score.clamp(0.0, 1.0);
    let corroboration = signals.corroboration.clamp(0.0, 1.0);
    let coverage = signals.corpus_coverage.clamp(0.0, 1.0);
    (0.5 * self_score + 0.3 * corroboration + 0.2 * coverage).clamp(0.0, 1.0)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── BiographerOutput shape ──────────────────────────────────────────

    /// The first-two-fields invariant (`confidence` then `rationale`) is
    /// load-bearing for streaming early-exit per #23. A regression that
    /// reorders fields would silently break that path. Pin the order via
    /// the serialized JSON.
    #[test]
    fn biographer_output_field_order_is_streaming_compatible() {
        let out = BiographerOutput {
            confidence: 0.9,
            rationale: "well-grounded".into(),
            sparse_content_gate: false,
            sections: BiographySections::default(),
        };
        let json = serde_json::to_string(&out).expect("serialize");
        // `confidence` must be the very first key — the streaming consumer
        // wants to read it before paying for tokens of any other field.
        let confidence_pos = json.find("\"confidence\"").expect("has confidence");
        let rationale_pos = json.find("\"rationale\"").expect("has rationale");
        let sections_pos = json.find("\"sections\"").expect("has sections");
        assert_eq!(
            confidence_pos, 1,
            "confidence must be first key, got json: {json}"
        );
        assert!(
            confidence_pos < rationale_pos,
            "confidence must precede rationale"
        );
        assert!(
            rationale_pos < sections_pos,
            "rationale must precede sections"
        );
    }

    /// The stub is what the host produces when the gate trips without
    /// spending tokens. Verify its invariants: the gate flag is `true`,
    /// every section is empty, the rationale carries the reason.
    #[test]
    fn sparse_stub_carries_gate_flag_and_empty_sections() {
        let stub = BiographerOutput::sparse_stub("only 85 human notes; need 200");
        assert!(stub.sparse_content_gate, "stub must mark the gate tripped");
        assert!(
            stub.rationale.contains("85"),
            "rationale must carry the specific reason"
        );
        let s = &stub.sections;
        for (name, content) in [
            ("identity", &s.identity),
            ("domains", &s.domains_of_expertise),
            ("themes", &s.recurring_themes),
            ("commitments", &s.stated_commitments),
            ("questions", &s.open_questions),
            ("drift", &s.drift_since_last_update),
        ] {
            assert!(
                content.is_empty(),
                "stub section `{name}` must be empty, got: {content:?}"
            );
        }
        // Stub confidence is 1.0 — high confidence in the *abstention*.
        // See the type's docs for why this is correct.
        assert!((stub.confidence - 1.0).abs() < f32::EPSILON);
    }

    /// Round-trip the LLM's expected output shape. Catches accidental
    /// breakage of the `Deserialize` impl (renamed field, changed type).
    #[test]
    fn biographer_output_deserializes_from_canonical_json() {
        let json = r#"{
            "confidence": 0.78,
            "rationale": "Five domains corroborated; drift mild.",
            "sparse_content_gate": false,
            "sections": {
                "identity": "Independent thinker, writes daily.",
                "domains_of_expertise": "- distributed systems\n- writing",
                "recurring_themes": "- attention\n- legibility",
                "stated_commitments": "- ship daily",
                "open_questions": "- how to balance depth and breadth",
                "drift_since_last_update": "Shifted from theory to practice."
            }
        }"#;
        let out: BiographerOutput = serde_json::from_str(json).expect("parse");
        assert!((out.confidence - 0.78).abs() < 1e-6);
        assert!(out.rationale.contains("corroborated"));
        assert!(!out.sparse_content_gate);
        assert_eq!(out.sections.identity, "Independent thinker, writes daily.");
        assert!(out.sections.domains_of_expertise.contains("distributed"));
    }

    /// Schema discipline: per the house convention (`deny_unknown_fields`),
    /// an output carrying a field outside the documented schema is
    /// *rejected*, not silently accepted. This is what `super::validate`
    /// relies on to catch prompt/struct drift in eval and CI. (The runner's
    /// hot path stays permissive via `parse_confidence`, so this strictness
    /// never takes down the gate at runtime.)
    #[test]
    fn unknown_fields_in_output_are_rejected() {
        let json = r#"{
            "confidence": 0.5,
            "rationale": "x",
            "sparse_content_gate": false,
            "sections": {
                "identity": "", "domains_of_expertise": "",
                "recurring_themes": "", "stated_commitments": "",
                "open_questions": "", "drift_since_last_update": ""
            },
            "future_field": "should be rejected"
        }"#;
        let err = serde_json::from_str::<BiographerOutput>(json)
            .expect_err("deny_unknown_fields must reject extra keys");
        assert!(
            err.to_string().contains("future_field"),
            "error should name the offending field, got: {err}"
        );
    }

    // ── SparseContentGate logic ─────────────────────────────────────────

    /// Default thresholds match the design doc (200 notes, 60 days). A
    /// regression that changes the default silently would alter every
    /// agent's gate behaviour; pin the values.
    #[test]
    fn default_thresholds_match_design_doc() {
        let g = SparseContentGate::default();
        assert_eq!(g.min_human_notes, 200);
        assert_eq!(g.min_vault_age_days, 60);
    }

    /// At-threshold passes (not strictly less than). The design says
    /// "≥ 200 notes" and "≥ 60 days" — boundary case.
    #[test]
    fn gate_passes_exactly_at_threshold() {
        let g = SparseContentGate::default();
        assert!(g
            .should_abstain(VaultSnapshot {
                human_notes_total: 200,
                age_days: 60,
            })
            .is_none());
    }

    /// One short by 1 is an abstention. Catches off-by-one in the
    /// comparison.
    #[test]
    fn gate_abstains_one_short_on_either_axis() {
        let g = SparseContentGate::default();
        let r_notes = g.should_abstain(VaultSnapshot {
            human_notes_total: 199,
            age_days: 60,
        });
        assert!(
            r_notes.is_some(),
            "one note short must abstain, got: {r_notes:?}"
        );
        assert!(r_notes.as_ref().unwrap().contains("199"));

        let r_age = g.should_abstain(VaultSnapshot {
            human_notes_total: 200,
            age_days: 59,
        });
        assert!(
            r_age.is_some(),
            "one day short must abstain, got: {r_age:?}"
        );
        assert!(r_age.as_ref().unwrap().contains("59"));
    }

    /// Both axes short produces a reason that mentions both — important
    /// for the proposal's rationale, so the user sees the full picture.
    #[test]
    fn gate_short_on_both_axes_mentions_both_in_reason() {
        let g = SparseContentGate::default();
        let r = g
            .should_abstain(VaultSnapshot {
                human_notes_total: 50,
                age_days: 10,
            })
            .expect("must abstain");
        assert!(r.contains("50"));
        assert!(r.contains("10"));
        assert!(r.contains("200"));
        assert!(r.contains("60"));
    }

    /// Custom thresholds (e.g. test harness wants smaller floors)
    /// override defaults cleanly.
    #[test]
    fn custom_thresholds_are_honoured() {
        let g = SparseContentGate {
            min_human_notes: 5,
            min_vault_age_days: 7,
        };
        assert!(g
            .should_abstain(VaultSnapshot {
                human_notes_total: 5,
                age_days: 7,
            })
            .is_none());
        assert!(g
            .should_abstain(VaultSnapshot {
                human_notes_total: 4,
                age_days: 7,
            })
            .is_some());
    }

    // ── compute_confidence ──────────────────────────────────────────────

    /// Pinned formula output for a representative input. Catches silent
    /// coefficient drift — if anyone changes the weights, this test
    /// surfaces it immediately.
    #[test]
    fn formula_pins_canonical_weights() {
        let c = compute_confidence(ConfidenceSignals {
            llm_self_score: 0.8,
            corroboration: 0.6,
            corpus_coverage: 0.5,
        });
        // 0.5*0.8 + 0.3*0.6 + 0.2*0.5 = 0.40 + 0.18 + 0.10 = 0.68
        assert!(
            (c - 0.68).abs() < 1e-5,
            "formula drifted: expected 0.68, got {c}"
        );
    }

    /// Clamping floor at 0.0 — even with all-zero signals, output is
    /// non-negative. Guards against accidental subtraction creeping into
    /// the formula in future.
    #[test]
    fn formula_clamps_at_zero_floor() {
        let c = compute_confidence(ConfidenceSignals {
            llm_self_score: -1.0,
            corroboration: -1.0,
            corpus_coverage: -1.0,
        });
        assert_eq!(c, 0.0);
    }

    /// Clamping ceiling at 1.0 — over-the-top inputs do not exceed the
    /// declared output range. Downstream consumers (Watcher, scorecards)
    /// rely on `[0.0, 1.0]`.
    #[test]
    fn formula_clamps_at_one_ceiling() {
        let c = compute_confidence(ConfidenceSignals {
            llm_self_score: 2.0,
            corroboration: 2.0,
            corpus_coverage: 2.0,
        });
        assert_eq!(c, 1.0);
    }

    /// Monotonicity in the LLM-self-score axis. Holding deterministic
    /// signals fixed, a higher self-score must yield ≥ confidence. This
    /// guards the "0.5 weight on self-score" assumption against future
    /// refactors that flip a sign.
    #[test]
    fn formula_is_monotonic_in_self_score() {
        let mk = |s| ConfidenceSignals {
            llm_self_score: s,
            corroboration: 0.5,
            corpus_coverage: 0.5,
        };
        let low = compute_confidence(mk(0.1));
        let mid = compute_confidence(mk(0.5));
        let high = compute_confidence(mk(0.9));
        assert!(low <= mid, "low={low} mid={mid}");
        assert!(mid <= high, "mid={mid} high={high}");
    }
}
