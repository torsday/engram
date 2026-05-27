//! Adapter that bridges [`crate::runner::AgentRunner`] into the
//! [`engram_eval::Invoker`] contract.
//!
//! The eval framework's runner takes an `Invoker` closure that
//! receives a [`engram_eval::Case`] + the path to a seeded vault
//! and returns an [`engram_eval::Observation`]. This module exports
//! the production adapter that wires a real [`AgentRunner`] into
//! that contract:
//!
//! 1. The closure invokes
//!    `runner.run_agent(case.id_as_agent_name, TriggerContext::OnDemand { … })`
//!    (the agent name is read from a separate parameter — see below).
//! 2. The resulting [`crate::runner::RunReport`] is mapped to an
//!    [`engram_eval::Observation`]:
//!    - `confidence` ← report.confidence
//!    - `rationale` ← report.rationale
//!    - `cost_usd`  ← report.cost_cents / 100.0
//!    - `proposed_link_targets` ← regex-scan of report.response_text
//!      for `[[wikilink]]` patterns. Cheap heuristic; the structured
//!      version (extracting targets from
//!      [`crate::runner::ProposedChange.new_content`] via the AST
//!      walker) is a future slice.
//! 3. `RunnerError` is converted to an
//!    [`engram_eval::InvokerError`] so the eval runner records the
//!    case as `Verdict::Error` rather than aborting.
//!
//! # Agent identity
//!
//! The adapter takes the `agent_name` at construction time. Every
//! case the resulting invoker handles runs against this one agent —
//! which matches how the eval framework is laid out
//! (`.engram/evals/<agent>/cases/`).

use std::sync::Arc;

use engram_eval::{Invoker, InvokerError, Observation};

use crate::runner::{AgentRunner, TriggerContext};

/// Build an [`Invoker`] closure that drives a real [`AgentRunner`].
///
/// `agent_name` is the agent under evaluation. `runner` is shared
/// (via `Arc`) so the closure can outlive the constructor's stack
/// frame; the eval framework boxes it into a `Box<dyn Fn + Send + Sync>`.
///
/// The closure is `Send + Sync` because:
/// - `Arc<AgentRunner>` is `Send + Sync`,
/// - the captured `agent_name` is owned `String`.
pub fn agent_runner_invoker(runner: Arc<AgentRunner>, agent_name: impl Into<String>) -> Invoker {
    let agent_name = agent_name.into();
    Box::new(move |case, _vault_path| {
        let trigger = TriggerContext::OnDemand {
            note_id: case.input.trigger_note_id.clone(),
        };
        // Bridge sync → async: the eval Invoker is sync, but
        // run_agent is async. Spin up a private current-thread
        // runtime per call rather than dragging tokio-runtime
        // mechanics through the synchronous Invoker contract.
        // Cheap enough — eval runs are once-per-case, not hot-path.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| InvokerError::new(format!("runtime build failed: {e}")))?;
        let report = rt
            .block_on(runner.run_agent(&agent_name, trigger))
            .map_err(|e| InvokerError::new(format!("run_agent failed: {e}")))?;
        Ok(report_to_observation(&report))
    })
}

/// Pure mapping `RunReport → Observation`. Exposed `pub(crate)`
/// so the adapter's tests can exercise it without spinning up the
/// runner stack.
pub(crate) fn report_to_observation(report: &crate::runner::RunReport) -> Observation {
    Observation {
        proposed_link_targets: extract_wikilink_targets(&report.response_text),
        confidence: report.confidence.map(|c| c as f64),
        rationale: report.rationale.clone(),
        cost_usd: report.cost_cents / 100.0,
    }
}

/// Scan `s` for `[[target]]` wikilink patterns and return the
/// targets (the part before any `|` alias or `#^block-id` suffix).
/// Cheap byte-level scanner — no comrak. Order-preserving;
/// duplicates kept (the scorer collapses if necessary).
fn extract_wikilink_targets(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let start = i + 2;
            // Find closing ]] from start.
            let mut j = start;
            while j + 1 < bytes.len() {
                if bytes[j] == b']' && bytes[j + 1] == b']' {
                    break;
                }
                j += 1;
            }
            if j + 1 < bytes.len() && bytes[j] == b']' && bytes[j + 1] == b']' {
                if let Ok(inner) = std::str::from_utf8(&bytes[start..j]) {
                    // Strip the `|alias` and `#^block-id` suffixes.
                    let target = inner
                        .split('|')
                        .next()
                        .unwrap_or("")
                        .split("#^")
                        .next()
                        .unwrap_or("")
                        .trim();
                    if !target.is_empty() {
                        out.push(target.to_string());
                    }
                }
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{ProposedChange, RunOutcome, RunReport, WriteResult};

    fn report(
        confidence: Option<f32>,
        rationale: Option<&str>,
        cost_cents: f64,
        response_text: &str,
    ) -> RunReport {
        RunReport {
            run_id: "01R".into(),
            correlation_id: "01C".into(),
            agent: "linker".into(),
            outcome: RunOutcome::NoAction,
            input_tokens: 0,
            output_tokens: 0,
            cost_cents,
            confidence,
            invasiveness: None,
            kind: None,
            rationale: rationale.map(String::from),
            proposed_changes: Vec::<ProposedChange>::new(),
            path_validation: Vec::new(),
            write_results: Vec::<WriteResult>::new(),
            response_text: response_text.into(),
            action_id: None,
            proposal_id: None,
            sub_agent_depth: 0,
        }
    }

    #[test]
    fn cost_cents_converts_to_usd() {
        let r = report(None, None, 250.0, "");
        let obs = report_to_observation(&r);
        assert!((obs.cost_usd - 2.50).abs() < 1e-9);
    }

    #[test]
    fn confidence_and_rationale_propagate() {
        let r = report(Some(0.87), Some("strong overlap"), 0.0, "");
        let obs = report_to_observation(&r);
        // f32→f64 conversion expands the representation, so use a
        // tolerance instead of an exact compare.
        assert!((obs.confidence.unwrap() - 0.87).abs() < 1e-6);
        assert_eq!(obs.rationale.as_deref(), Some("strong overlap"));
    }

    #[test]
    fn extract_wikilink_targets_finds_simple_links() {
        assert_eq!(
            extract_wikilink_targets("see [[alpha]] and [[beta]]"),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn extract_wikilink_targets_strips_alias_after_pipe() {
        assert_eq!(
            extract_wikilink_targets("see [[Target Note|alias]]"),
            vec!["Target Note".to_string()]
        );
    }

    #[test]
    fn extract_wikilink_targets_strips_block_id() {
        assert_eq!(
            extract_wikilink_targets("see [[Note#^block-1]]"),
            vec!["Note".to_string()]
        );
    }

    #[test]
    fn extract_wikilink_targets_handles_empty_and_malformed() {
        assert_eq!(extract_wikilink_targets(""), Vec::<String>::new());
        // Single `[` is not a wikilink.
        assert_eq!(extract_wikilink_targets("[alpha]"), Vec::<String>::new());
        // Unclosed `[[` is ignored.
        assert_eq!(
            extract_wikilink_targets("text [[unfinished"),
            Vec::<String>::new()
        );
        // Empty target.
        assert_eq!(extract_wikilink_targets("[[]]"), Vec::<String>::new());
    }

    #[test]
    fn extract_wikilink_targets_preserves_duplicates_in_order() {
        assert_eq!(
            extract_wikilink_targets("[[a]] [[b]] [[a]] [[c]]"),
            vec![
                "a".to_string(),
                "b".to_string(),
                "a".to_string(),
                "c".to_string()
            ]
        );
    }

    #[test]
    fn report_to_observation_extracts_targets_from_response_text() {
        let r = report(
            Some(0.9),
            Some("rationale"),
            100.0,
            r#"{"proposed_link_targets":[]}; see also [[alpha]] and [[beta|alias]]"#,
        );
        let obs = report_to_observation(&r);
        assert_eq!(obs.proposed_link_targets, vec!["alpha", "beta"]);
    }
}
