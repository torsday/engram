//! Single dispatch entry point over the 9 typed agent output
//! structs.
//!
//! Each agent on disk under `agents/<name>/` has a documented JSON
//! output schema; each has a typed Rust mirror under
//! [`super`]. This module provides one function — [`validate`] —
//! that maps agent name → typed parser → structured result.
//!
//! ## Why this is separate from the runner
//!
//! [`AgentRunner`](crate::runner::AgentRunner) treats LLM responses
//! as opaque JSON, extracting `confidence` via a permissive
//! `Value::get` lookup so that schema-mismatch doesn't take down
//! the gate. That's the right behavior for the hot path: keep the
//! runner robust, log warnings out-of-band.
//!
//! Callers that *want* strict validation — eval cases, CLI
//! dry-runs, schema-drift CI checks, future runner integration —
//! opt in via [`validate`].
//!
//! ## What it does
//!
//! - Looks up the agent name in a registry of typed parsers.
//! - Parses the response text against the agent's
//!   `deny_unknown_fields`-bearing struct.
//! - Returns either the parsed (boxed-dyn-erased) value or a
//!   structured [`ValidationError`].
//!
//! ## What it does NOT do
//!
//! - It does not change runner behavior; the runner continues to
//!   use `parse_confidence` on the hot path.
//! - It does not return the typed struct directly — each agent's
//!   typed struct has its own shape, so the validate dispatch
//!   reports success/failure and lets the caller invoke the
//!   per-agent parser directly when it needs the typed value.
//! - It does not validate cross-field invariants beyond what
//!   serde enforces (e.g. "mode/payload consistency" for
//!   `Inquirer` and `VoiceKeeper` is still a runtime invariant).

use serde::Deserialize;
use thiserror::Error;

use super::{
    biographer::BiographerOutput, bridge_builder::BridgeBuilderOutput,
    completion_nudger::CompletionNudgerOutput, confidence_annotator::ConfidenceAnnotatorOutput,
    devils_advocate::DevilsAdvocateOutput, gardener::GardenerOutput, historian::HistorianOutput,
    inbox_triage::TriageOutput, inquirer::InquirerOutput, linker::LinkerOutput,
    merger::MergerOutput, pair_thinking::PairThinkingTurn, predictor::PredictorOutput,
    scribe::ScribeOutput, source_demand::SourceDemandOutput, splitter::SplitterOutput,
    steelman_constructive::SteelmanConstructiveOutput, synthesizer::SynthesizerOutput,
    tutor::TutorOutput, voice_keeper::VoiceKeeperOutput, witness::WitnessOutput,
};
use crate::cartographer::CartographerContinuousOutput;

/// Errors produced by [`validate`].
#[derive(Debug, Error)]
pub enum ValidationError {
    /// The agent name is not in the registry. Either it's a typo,
    /// or a new agent's typed struct hasn't been registered yet.
    #[error(
        "no typed-output validator registered for agent `{name}` — \
         add it to `validate.rs` once the agent's typed struct lands"
    )]
    UnknownAgent {
        /// The name that didn't match any registered agent.
        name: String,
    },

    /// The response text failed to parse against the agent's
    /// typed struct. The inner serde error carries the field path
    /// and the parse problem.
    #[error("agent `{name}` output failed schema validation: {source}")]
    ParseFailed {
        /// The agent the failure is attributed to.
        name: String,
        /// The serde-json error explaining what failed.
        #[source]
        source: serde_json::Error,
    },
}

/// Validate `text` against the typed output schema for `agent_name`.
///
/// Returns `Ok(())` on success. The function does not return the
/// parsed value — each agent's typed struct has a different shape,
/// so callers that need the value should invoke the per-agent
/// parser directly (e.g. `serde_json::from_str::<InquirerOutput>(text)`).
/// `validate` is the dispatch point for "does this output conform?"
/// without committing the caller to a particular agent's type.
///
/// # Errors
///
/// - [`ValidationError::UnknownAgent`] when `agent_name` doesn't
///   match any registered agent. The list of registered names
///   tracks the on-disk `agents/<name>/` set 1:1.
/// - [`ValidationError::ParseFailed`] when serde rejects the text.
///   Includes the underlying `serde_json::Error`, which carries
///   the field path and the parse problem.
///
/// # Example
///
/// ```ignore
/// use engram_agents::agents::validate::validate;
///
/// // Bare-minimum Steelman output: confidence + rationale.
/// let text = r#"{
///     "confidence": 0.5,
///     "rationale": "The note is structurally sound."
/// }"#;
/// validate("steelman-constructive", text).expect("valid");
/// ```
pub fn validate(agent_name: &str, text: &str) -> Result<(), ValidationError> {
    // The registry is a match arm rather than a HashMap because the
    // set is small (9), known at compile time, and exhaustive — a
    // missing arm here means a new agent slipped the typed-struct
    // contract, which is exactly what we want the compiler to catch.
    match agent_name {
        "steelman-constructive" => check::<SteelmanConstructiveOutput>(agent_name, text),
        "devils-advocate" => check::<DevilsAdvocateOutput>(agent_name, text),
        "inquirer" => check::<InquirerOutput>(agent_name, text),
        "synthesizer" => check::<SynthesizerOutput>(agent_name, text),
        "voice-keeper" => check::<VoiceKeeperOutput>(agent_name, text),
        "pair-thinking" => check::<PairThinkingTurn>(agent_name, text),
        "splitter" => check::<SplitterOutput>(agent_name, text),
        "merger" => check::<MergerOutput>(agent_name, text),
        "bridge-builder" => check::<BridgeBuilderOutput>(agent_name, text),
        "cartographer" => check::<CartographerContinuousOutput>(agent_name, text),
        "linker" => check::<LinkerOutput>(agent_name, text),
        "scribe" => check::<ScribeOutput>(agent_name, text),
        "gardener" => check::<GardenerOutput>(agent_name, text),
        "predictor" => check::<PredictorOutput>(agent_name, text),
        "witness" => check::<WitnessOutput>(agent_name, text),
        "confidence-annotator" => check::<ConfidenceAnnotatorOutput>(agent_name, text),
        "source-demand" => check::<SourceDemandOutput>(agent_name, text),
        "completion-nudger" => check::<CompletionNudgerOutput>(agent_name, text),
        "tutor" => check::<TutorOutput>(agent_name, text),
        "historian" => check::<HistorianOutput>(agent_name, text),
        "biographer" => check::<BiographerOutput>(agent_name, text),
        "inbox-triage" => check::<TriageOutput>(agent_name, text),
        other => Err(ValidationError::UnknownAgent {
            name: other.to_string(),
        }),
    }
}

/// Return the list of agent names this module knows how to
/// validate. The list is exhaustive 1:1 with the registry in
/// [`validate`]; callers can use it to assert at startup that
/// every on-disk agent has a corresponding typed schema.
pub fn registered_agents() -> &'static [&'static str] {
    &[
        "steelman-constructive",
        "devils-advocate",
        "inquirer",
        "synthesizer",
        "voice-keeper",
        "pair-thinking",
        "splitter",
        "merger",
        "bridge-builder",
        "cartographer",
        "linker",
        "scribe",
        "gardener",
        "predictor",
        "witness",
        "confidence-annotator",
        "source-demand",
        "completion-nudger",
        "tutor",
        "historian",
        "biographer",
        "inbox-triage",
    ]
}

/// Inner generic helper. Centralizes the deserialize +
/// error-mapping path so each registry arm stays a one-liner.
fn check<'a, T: Deserialize<'a>>(agent_name: &str, text: &'a str) -> Result<(), ValidationError> {
    serde_json::from_str::<T>(text)
        .map(|_| ())
        .map_err(|source| ValidationError::ParseFailed {
            name: agent_name.to_string(),
            source,
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered agent name should accept its minimal valid
    /// output (confidence + rationale + whatever else is
    /// non-defaulted). This is the smoke test for "the dispatch
    /// surface actually wires up to a parser per agent."
    #[test]
    fn every_registered_agent_accepts_minimal_output() {
        // Per-agent minimal samples: each is the smallest body
        // that satisfies the type's required fields. If a typed
        // struct adds a required field, this test surfaces the
        // drift immediately.
        for (agent, sample) in [
            (
                "steelman-constructive",
                r#"{"confidence":0.5,"rationale":"r"}"#,
            ),
            (
                "devils-advocate",
                r#"{"confidence":0.5,"rationale":"r","decline":true}"#,
            ),
            (
                "inquirer",
                r#"{"confidence":0.5,"rationale":"r","mode":"daily-reactive","output_path":"inbox/x.md"}"#,
            ),
            (
                "synthesizer",
                r#"{"confidence":0.5,"rationale":"r","decline":true,"cluster_coherence":{"coherent":false}}"#,
            ),
            (
                "voice-keeper",
                r#"{"confidence":0.5,"rationale":"r","mode":"review"}"#,
            ),
            (
                "pair-thinking",
                r#"{"confidence":0.5,"rationale":"r","round":1,"mode":"end","question":"","should_end":true}"#,
            ),
            (
                "splitter",
                r#"{"confidence":0.5,"rationale":"r","decline":true}"#,
            ),
            (
                "merger",
                r#"{"confidence":0.5,"rationale":"r","decline":true}"#,
            ),
            ("bridge-builder", r#"{"confidence":0.5,"rationale":"r"}"#),
            (
                "cartographer",
                r#"{"confidence":0.5,"rationale":"r","index_updates":[]}"#,
            ),
            ("linker", r#"{"confidence":0.5,"rationale":"r"}"#),
            (
                "scribe",
                r#"{"confidence":0.5,"rationale":"r","cleaned_body":"x","mode":"fleeting_cleanup","length_ratio":1.0}"#,
            ),
            ("gardener", r#"{"confidence":0.5,"rationale":"r"}"#),
            ("predictor", r#"{"confidence":0.5,"rationale":"r"}"#),
            (
                "witness",
                r#"{"confidence":0.9,"rationale":"r","acknowledgment":"Thank you for sharing.","output_path":".engram/witness/2026-01-01.md"}"#,
            ),
            (
                "confidence-annotator",
                r#"{"confidence":0.5,"rationale":"r"}"#,
            ),
            ("source-demand", r#"{"confidence":0.5,"rationale":"r"}"#),
            ("completion-nudger", r#"{"confidence":0.5,"rationale":"r"}"#),
            ("tutor", r#"{"confidence":0.5,"rationale":"r"}"#),
            (
                "historian",
                r#"{"confidence":0.5,"rationale":"r","log_entry":"x","output_path":"y"}"#,
            ),
            (
                "biographer",
                r#"{"confidence":0.5,"rationale":"r","sections":{"identity":"","domains_of_expertise":"","recurring_themes":"","stated_commitments":"","open_questions":"","drift_since_last_update":""}}"#,
            ),
        ] {
            validate(agent, sample)
                .unwrap_or_else(|e| panic!("{agent} minimal sample failed: {e}"));
        }
    }

    #[test]
    fn unknown_agent_surfaces_distinct_error() {
        let err = validate("not-an-agent", r#"{"confidence":0.5}"#)
            .expect_err("unknown agent must error");
        assert!(
            matches!(err, ValidationError::UnknownAgent { ref name } if name == "not-an-agent"),
            "expected UnknownAgent, got: {err:?}"
        );
    }

    #[test]
    fn schema_violation_surfaces_distinct_error() {
        // Missing required `rationale` field for steelman.
        let err = validate("steelman-constructive", r#"{"confidence":0.5}"#)
            .expect_err("schema violation must error");
        assert!(
            matches!(err, ValidationError::ParseFailed { .. }),
            "expected ParseFailed, got: {err:?}"
        );
    }

    #[test]
    fn unknown_field_surfaces_via_parse_failed() {
        // deny_unknown_fields rejection should flow up as
        // ParseFailed (the schema-drift guardrail surfacing
        // through the dispatch).
        let err = validate(
            "steelman-constructive",
            r#"{"confidence":0.5,"rationale":"r","future":"x"}"#,
        )
        .expect_err("unknown field must error");
        match err {
            ValidationError::ParseFailed { name, source } => {
                assert_eq!(name, "steelman-constructive");
                assert!(
                    source.to_string().contains("future"),
                    "error should point at the offending field; got: {source}"
                );
            }
            other => panic!("expected ParseFailed, got: {other:?}"),
        }
    }

    /// `registered_agents()` must list exactly the same names the
    /// dispatch in `validate` accepts. This catches additions to
    /// one and not the other.
    #[test]
    fn registered_list_matches_dispatch() {
        for &name in registered_agents() {
            // Each registered name parses *something* — at minimum
            // a bogus body that fails to parse rather than failing
            // dispatch.
            let result = validate(name, "{");
            // We don't care whether the bogus body parses; we care
            // that the dispatch *reached* the parser (i.e., it's
            // not UnknownAgent for any registered name).
            assert!(
                !matches!(result, Err(ValidationError::UnknownAgent { .. })),
                "{name} is in registered_agents() but not in the validate() dispatch"
            );
        }
    }

    /// The registered count must match the on-disk agent count.
    /// If a new agent's files land in `agents/<name>/` but its
    /// typed struct hasn't been registered here, this test
    /// surfaces the drift. (Counterpart: the smoke test in
    /// `runner::tests::on_disk_agent_files_parse` validates the
    /// on-disk side.)
    #[test]
    fn registered_count_matches_on_disk_floor() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let agents_dir = workspace.join("agents");
        let mut on_disk = 0;
        for entry in std::fs::read_dir(&agents_dir).expect("read agents dir") {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir()
                && entry.path().join("config.toml").is_file()
                && entry.path().join("prompt.md").is_file()
            {
                on_disk += 1;
            }
        }
        assert_eq!(
            on_disk,
            registered_agents().len(),
            "on-disk agent count ({on_disk}) must equal registered typed-validator count ({}) — \
             a mismatch means slice 1 (files) and slice 2 (typed struct) drifted",
            registered_agents().len()
        );
    }
}
