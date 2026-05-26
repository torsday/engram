//! Agent runner — the central runtime that loads an agent, assembles a prompt,
//! invokes the LLM, and records the invocation to `agent_runs`.
//!
//! This module is the first slice of [#27]. It ships the **single-shot
//! `on_demand` path**: one invocation = one LLM call = one `agent_runs` row.
//! The scheduler, file-watcher dispatch, cron loop, lock acquisition wiring,
//! invasiveness classifier, decision matrix (write unstaged vs. file
//! proposal), and `engram run` CLI subcommand each land as their own
//! follow-ups so each can be reviewed in isolation.
//!
//! # What this slice gives you
//!
//! - [`AgentRunner::new`] — construct around a SQLite connection, an
//!   `LlmProvider`, the embedding/completion `Model`, and the on-disk
//!   `agents/` directory root.
//! - [`AgentRunner::run_agent`] — load `agents/<name>/config.toml` + `prompt.md`,
//!   render the dynamic tail against the trigger context, call the provider,
//!   parse the response's `confidence` field, write the `agent_runs` row,
//!   return a [`RunReport`].
//! - [`AgentConfig`] — minimal config schema (just the fields the runner needs
//!   for this slice). Future slices extend it without changing the entry point.
//! - [`TriggerKind`] / [`TriggerContext`] — config-side declaration vs.
//!   runtime invocation argument.
//! - [`RunReport`] / [`RunOutcome`] — what the runner returns.
//!
//! # What this slice does NOT do
//!
//! Each deferred AC item is filed as its own follow-up issue so progress on
//! `#27` is incremental and reviewable:
//!
//! - **Scheduler / file-watcher / cron** — `start_scheduler`, `dispatch`,
//!   tokio-interval per-agent loops
//! - **Lock acquisition** — [`crate::locks::LockManager`] exists; wiring is
//!   one step in `run_agent` once we know what trigger contexts name a note
//! - **Invasiveness classifier + decision matrix** — confidence-and-invasiveness
//!   gating that writes unstaged via atomic_writes OR files a proposal
//! - **`agent_actions` recording** — the action-log row that lands when a
//!   write happens (depends on the decision matrix)
//! - **Hot-reload semantics** — currently re-reads config + prompt every call
//!   (the simplest hot-reload), but ADR-style hot-reload that holds the
//!   loaded prompt for in-flight runs is a separate slice
//! - **`engram run <agent>` CLI** — a thin wrapper around `run_agent`
//! - **Streaming + escalation wrapping** — uses non-streaming `complete()`
//!   with whatever provider the caller passes; the
//!   [`crate::EscalatingProvider`](engram_llm::EscalatingProvider) stack
//!   composes externally
//!
//! # Correlation IDs
//!
//! Every invocation generates a `correlation_id` (a ULID, distinct from the
//! `run_id`). It's emitted as a `tracing` span attribute on every span in
//! the agent's call tree, so traces collected during a run can be filtered
//! to a single invocation. The `run_id` is the `agent_runs` PK.
//!
//! [#27]: https://github.com/torsday/engram/issues/27

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use engram_core::note_id::NoteId;
use engram_llm::{CompleteOptions, Error as LlmError, LlmProvider, Model, PromptStructured};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info_span, Instrument};

use crate::prompt_loader::{self, PromptLoadError};

/// Errors raised by [`AgentRunner::run_agent`].
#[derive(Debug, Error)]
pub enum RunnerError {
    /// The `agents/<name>/` directory or one of its required files is
    /// missing.
    #[error("agent `{name}` not found: {detail}")]
    AgentNotFound {
        /// Agent name as supplied by the caller.
        name: String,
        /// Why the lookup failed (missing directory, missing prompt, etc.).
        detail: String,
    },

    /// The agent's `config.toml` failed to parse.
    #[error("agent `{name}` config invalid: {source}")]
    ConfigInvalid {
        /// Agent name.
        name: String,
        /// Underlying parse error.
        #[source]
        source: toml::de::Error,
    },

    /// The agent's `prompt.md` failed to load.
    #[error("agent `{name}` prompt invalid: {source}")]
    PromptInvalid {
        /// Agent name.
        name: String,
        /// Underlying load error.
        #[source]
        source: PromptLoadError,
    },

    /// SQLite write to `agent_runs` failed.
    #[error("agent_runs write failed: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Reading from the filesystem failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Outcome recorded in `agent_runs.outcome` and returned in [`RunReport`].
///
/// This slice records `AutoLand`, `NoAction`, and `Errored` — the
/// `CouncilConvened` outcome lands when the council-convening branch of
/// the decision matrix is implemented (separate follow-up).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Confidence cleared the agent's threshold; in a full implementation
    /// this would write unstaged changes. Today the runner emits the
    /// outcome and leaves the actual write to a follow-up slice.
    AutoLand,
    /// Confidence was below threshold (would file a proposal in the full
    /// flow).
    NoAction,
    /// Council convened — placeholder; not produced by this slice.
    CouncilConvened,
    /// The provider returned an error or the response was malformed.
    Errored,
}

impl RunOutcome {
    fn as_sql(&self) -> &'static str {
        match self {
            Self::AutoLand => "auto_land",
            Self::NoAction => "no_action",
            Self::CouncilConvened => "council_convened",
            Self::Errored => "errored",
        }
    }
}

/// Trigger kind declared in `agents/<name>/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// Subscribes to file-watcher events (dispatch wiring lands in a follow-up).
    FileChange,
    /// Tokio interval per the agent's schedule (loop lands in a follow-up).
    Cron,
    /// Invoked manually via API/CLI/MCP. This slice covers this path end-to-end.
    OnDemand,
    /// Never invoked directly; participates when the council convenes.
    CouncilOnly,
}

impl TriggerKind {
    fn as_sql(&self) -> &'static str {
        match self {
            Self::FileChange => "file_change",
            Self::Cron => "cron",
            Self::OnDemand => "on_demand",
            Self::CouncilOnly => "council",
        }
    }
}

/// Runtime context the caller passes into [`AgentRunner::run_agent`].
#[derive(Debug, Clone)]
pub enum TriggerContext {
    /// On-demand invocation, optionally targeting a note.
    OnDemand {
        /// Note ID this run is about, if any. Surfaced to the prompt as
        /// `{{note_id}}`.
        note_id: Option<String>,
    },
    /// File-change event (dispatch is a follow-up; this variant exists so
    /// the API doesn't need to change when the dispatcher lands).
    FileChange {
        /// Note whose file changed.
        note_id: String,
    },
    /// Scheduled invocation (no per-call context).
    Cron,
    /// Convened by the council.
    Council {
        /// Council deliberation ID.
        deliberation_id: String,
    },
}

impl TriggerContext {
    fn trigger_label(&self) -> &'static str {
        match self {
            Self::OnDemand { .. } => "on_demand",
            Self::FileChange { .. } => "file_change",
            Self::Cron => "cron",
            Self::Council { .. } => "council",
        }
    }

    fn note_id(&self) -> Option<&str> {
        match self {
            Self::OnDemand { note_id, .. } => note_id.as_deref(),
            Self::FileChange { note_id } => Some(note_id.as_str()),
            _ => None,
        }
    }

    fn deliberation_id(&self) -> Option<&str> {
        match self {
            Self::Council { deliberation_id } => Some(deliberation_id.as_str()),
            _ => None,
        }
    }
}

/// Minimal agent configuration parsed from `agents/<name>/config.toml`.
///
/// Only the fields the runner needs right now are required. Unknown keys
/// are accepted via `serde(default)` so adding fields in a future slice
/// doesn't reject existing configs.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    /// Agent name. Must match the directory name for `run_agent` to find
    /// the prompt; the runner does not enforce equality but it's a strong
    /// convention.
    pub name: String,
    /// Trigger kind. Drives dispatch (in future slices); the value is
    /// recorded in the `agent_runs.trigger` column for correlation.
    pub trigger: TriggerKind,
    /// Confidence floor. Responses whose `confidence` field is at or above
    /// this threshold produce [`RunOutcome::AutoLand`]; below produces
    /// [`RunOutcome::NoAction`]. Default `0.85`.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f32,
}

fn default_confidence_threshold() -> f32 {
    0.85
}

impl AgentConfig {
    /// Parse from a TOML string. Useful for tests and for callers that
    /// have already read the file.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// What `run_agent` returns and what the caller logs / surfaces.
#[derive(Debug, Clone, PartialEq)]
pub struct RunReport {
    /// `agent_runs.id` (ULID).
    pub run_id: String,
    /// Correlation ID propagated through tracing spans (ULID, distinct
    /// from `run_id`).
    pub correlation_id: String,
    /// Agent name as supplied.
    pub agent: String,
    /// Final outcome recorded in `agent_runs.outcome`.
    pub outcome: RunOutcome,
    /// Input tokens reported by the provider for this invocation.
    pub input_tokens: u32,
    /// Output tokens reported by the provider.
    pub output_tokens: u32,
    /// Total per-invocation cost in US cents, computed by the provider
    /// from `usage` against the static price table. `0.0` when the
    /// provider's model is not in the price table (e.g. test mocks) or
    /// when the call errored before producing a [`Cost`].
    pub cost_cents: f64,
    /// Confidence parsed out of the response, if present.
    pub confidence: Option<f32>,
    /// Raw response text (kept so callers can route to action-log /
    /// proposal-filer in follow-up slices).
    pub response_text: String,
}

/// The agent runner.
///
/// Hold one per process; `run_agent` is safe to call concurrently from
/// multiple tasks (each call acquires the SQLite mutex only for the row
/// writes).
pub struct AgentRunner {
    sqlite: Arc<Mutex<Connection>>,
    provider: Arc<dyn LlmProvider>,
    model: Model,
    agents_dir: PathBuf,
}

impl AgentRunner {
    /// Construct the runner.
    ///
    /// - `sqlite` — the same connection that owns the `agent_runs` table
    /// - `provider` — any `LlmProvider`; wrap with
    ///   [`engram_llm::EscalatingProvider`] / `CircuitBreakerProvider` /
    ///   `RetryProvider` externally to compose the resilience stack
    /// - `model` — the concrete `Model` to invoke for this runner
    ///   (per-tier selection is the escalating wrapper's job)
    /// - `agents_dir` — root of the `agents/` directory tree on disk
    pub fn new(
        sqlite: Arc<Mutex<Connection>>,
        provider: Arc<dyn LlmProvider>,
        model: Model,
        agents_dir: PathBuf,
    ) -> Self {
        Self {
            sqlite,
            provider,
            model,
            agents_dir,
        }
    }

    /// Run an agent once. See module docs for the per-invocation flow.
    pub async fn run_agent(
        &self,
        name: &str,
        trigger: TriggerContext,
    ) -> Result<RunReport, RunnerError> {
        let correlation_id = NoteId::new().as_str().to_string();
        let run_id = NoteId::new().as_str().to_string();

        let span = info_span!(
            "agent_run",
            agent = name,
            correlation_id = %correlation_id,
            run_id = %run_id,
            trigger = trigger.trigger_label(),
        );
        async move {
            self.run_agent_inner(name, trigger, run_id, correlation_id)
                .await
        }
        .instrument(span)
        .await
    }

    async fn run_agent_inner(
        &self,
        name: &str,
        trigger: TriggerContext,
        run_id: String,
        correlation_id: String,
    ) -> Result<RunReport, RunnerError> {
        // Load agent config + prompt before recording the run start so we
        // surface configuration errors loudly instead of leaving an
        // open-ended row.
        let config = self.load_config(name)?;
        let prompt = self.load_prompt(name)?;

        // INSERT agent_runs row with started_at; we'll UPDATE with
        // completed_at + outcome at the end.
        let started_at = Utc::now().to_rfc3339();
        let notes_affected = trigger
            .note_id()
            .map(|n| serde_json::to_string(&[n]).unwrap_or_else(|_| "[]".into()))
            .unwrap_or_else(|| "[]".into());
        {
            let conn = self.sqlite.lock().expect("sqlite mutex poisoned");
            conn.execute(
                "INSERT INTO agent_runs (id, agent_name, started_at, trigger, notes_affected, deliberation_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    run_id,
                    name,
                    started_at,
                    config.trigger.as_sql(),
                    notes_affected,
                    trigger.deliberation_id(),
                ],
            )?;
        }

        // Render the dynamic tail with the trigger's context. Today this
        // is a minimal `{{trigger}}` / `{{note_id}}` substitution; future
        // slices will assemble neighbors, biographer model, etc.
        let rendered_tail = render_dynamic_tail(&prompt, &trigger, &correlation_id);

        let final_prompt = PromptStructured {
            static_head: prompt.static_head.clone(),
            dynamic_tail: rendered_tail,
        };

        // Call the provider. On error, mark the run errored and propagate
        // a clean Result so callers see exactly what failed.
        let completion_result = self
            .provider
            .complete(&final_prompt, &self.model, &CompleteOptions::default())
            .await;

        let (outcome, input_tokens, output_tokens, cost_cents, confidence, response_text) =
            match completion_result {
                Ok(completion) => {
                    let confidence = parse_confidence(&completion.text);
                    let outcome = match confidence {
                        Some(c) if c >= config.confidence_threshold => RunOutcome::AutoLand,
                        _ => RunOutcome::NoAction,
                    };
                    (
                        outcome,
                        completion.usage.input_tokens_total,
                        completion.usage.output_tokens,
                        completion.cost.total_cents,
                        confidence,
                        completion.text,
                    )
                }
                Err(e) => {
                    // Provider failure — record as errored and surface the
                    // message in the response slot so the agent_runs row is
                    // a complete trail.
                    (
                        RunOutcome::Errored,
                        0,
                        0,
                        0.0,
                        None,
                        format!("provider error: {e}"),
                    )
                }
            };

        let errored = matches!(outcome, RunOutcome::Errored) as i64;
        let completed_at = Utc::now().to_rfc3339();
        {
            let conn = self.sqlite.lock().expect("sqlite mutex poisoned");
            conn.execute(
                "UPDATE agent_runs SET completed_at = ?1, outcome = ?2, \
                 input_tokens = ?3, output_tokens = ?4, cost_cents = ?5, errored = ?6 \
                 WHERE id = ?7",
                rusqlite::params![
                    completed_at,
                    outcome.as_sql(),
                    input_tokens as i64,
                    output_tokens as i64,
                    cost_cents,
                    errored,
                    run_id,
                ],
            )?;
        }

        Ok(RunReport {
            run_id,
            correlation_id,
            agent: name.to_string(),
            outcome,
            input_tokens,
            output_tokens,
            cost_cents,
            confidence,
            response_text,
        })
    }

    fn load_config(&self, name: &str) -> Result<AgentConfig, RunnerError> {
        let path = self.agents_dir.join(name).join("config.toml");
        let raw = std::fs::read_to_string(&path).map_err(|e| RunnerError::AgentNotFound {
            name: name.to_string(),
            detail: format!("config.toml at {}: {}", path.display(), e),
        })?;
        AgentConfig::from_toml(&raw).map_err(|source| RunnerError::ConfigInvalid {
            name: name.to_string(),
            source,
        })
    }

    fn load_prompt(&self, name: &str) -> Result<PromptStructured, RunnerError> {
        let path = self.agents_dir.join(name).join("prompt.md");
        prompt_loader::load(&path).map_err(|source| RunnerError::PromptInvalid {
            name: name.to_string(),
            source,
        })
    }
}

/// Render the prompt's dynamic tail with trigger-context substitutions.
///
/// Supported placeholders (extend in follow-ups):
///
/// - `{{trigger}}` — `on_demand` / `file_change` / `cron` / `council`
/// - `{{note_id}}` — present when the trigger names a note
/// - `{{correlation_id}}` — current invocation's correlation ULID
fn render_dynamic_tail(
    prompt: &PromptStructured,
    trigger: &TriggerContext,
    correlation_id: &str,
) -> String {
    let mut vars: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    vars.insert("trigger", trigger.trigger_label());
    if let Some(id) = trigger.note_id() {
        vars.insert("note_id", id);
    }
    vars.insert("correlation_id", correlation_id);
    prompt_loader::render_tail(&prompt.dynamic_tail, &vars)
}

/// Best-effort extraction of `confidence` (number in `[0, 1]`) from a JSON
/// response body. Non-JSON or missing-field cases return `None` so
/// downstream logic can apply its own default behaviour.
fn parse_confidence(text: &str) -> Option<f32> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("confidence")
        .and_then(|c| c.as_f64())
        .map(|c| c as f32)
}

// Reserve the LLM-error type for follow-ups that surface it more richly in
// `RunReport`. Today it's stringified into `response_text`.
#[allow(dead_code)]
fn _llm_error_marker() -> LlmError {
    LlmError::Decode("placeholder".into())
}

/// Compile-time guard: callers expect `Path::join` semantics for the agents
/// directory. Kept as a private helper for clarity even though it's a
/// one-liner today.
#[allow(dead_code)]
fn agent_dir(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use engram_index::sqlite::Migrator;
    use engram_llm::{
        CompleteOptions, Completion, Cost, EmbeddingModel, ModelProvider, StreamedCompletion, Usage,
    };
    use std::sync::Mutex as StdMutex;
    use tempfile::tempdir;

    /// Tiny scripted provider — returns a queued response per call. We
    /// don't pull `MockLlmProvider` here because its scripted mode keys on
    /// prompt hash; sequencing by call count is simpler for these tests.
    struct ScriptedProvider {
        responses: StdMutex<Vec<Result<&'static str, &'static str>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<Result<&'static str, &'static str>>) -> Self {
            Self {
                responses: StdMutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn complete(
            &self,
            _prompt: &PromptStructured,
            model: &Model,
            _options: &CompleteOptions,
        ) -> engram_llm::Result<Completion> {
            let next = {
                let mut q = self.responses.lock().unwrap();
                if q.is_empty() {
                    return Err(engram_llm::Error::Decode("script exhausted".into()));
                }
                q.remove(0)
            };
            match next {
                Ok(text) => Ok(Completion {
                    text: text.to_string(),
                    usage: Usage {
                        input_tokens_total: 100,
                        output_tokens: 25,
                        ..Default::default()
                    },
                    cost: Cost {
                        input_cents: 1.0,
                        cache_create_cents: 0.0,
                        cache_read_cents: 0.0,
                        output_cents: 2.0,
                        total_cents: 3.0,
                    },
                    model_used: format!("mock/{}", model.name),
                    latency_ms: 1,
                }),
                Err(msg) => Err(engram_llm::Error::Decode(msg.to_string())),
            }
        }

        async fn complete_streamed(
            &self,
            _prompt: &PromptStructured,
            _model: &Model,
            _options: &CompleteOptions,
        ) -> engram_llm::Result<StreamedCompletion> {
            unreachable!("not used by AgentRunner today")
        }

        async fn embed(
            &self,
            _text: &str,
            _model: &EmbeddingModel,
        ) -> engram_llm::Result<Vec<f32>> {
            unreachable!("not used by AgentRunner today")
        }
    }

    fn test_model() -> Model {
        Model {
            provider: ModelProvider::Anthropic,
            name: "test-model".to_string(),
        }
    }

    fn setup_sqlite() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn write_agent(root: &Path, name: &str, config_toml: &str, prompt_body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), config_toml).unwrap();
        std::fs::write(dir.join("prompt.md"), prompt_body).unwrap();
    }

    const DEMO_PROMPT: &str = "You are a tester.\n\n<!-- /cache -->\n\nTrigger: {{trigger}}\nNote: {{note_id}}\nCorrelation: {{correlation_id}}\n";

    /// Helper: snapshot the lifecycle columns of one `agent_runs` row.
    struct PersistedRun {
        agent_name: String,
        completed_at: Option<String>,
        outcome: Option<String>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cost_cents: Option<f64>,
        errored: i64,
    }

    fn agent_runs_row(sqlite: &Arc<Mutex<Connection>>, run_id: &str) -> PersistedRun {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT agent_name, completed_at, outcome, input_tokens, output_tokens, \
             cost_cents, errored FROM agent_runs WHERE id = ?1",
            rusqlite::params![run_id],
            |row| {
                Ok(PersistedRun {
                    agent_name: row.get(0)?,
                    completed_at: row.get(1)?,
                    outcome: row.get(2)?,
                    input_tokens: row.get(3)?,
                    output_tokens: row.get(4)?,
                    cost_cents: row.get(5)?,
                    errored: row.get(6)?,
                })
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn high_confidence_response_lands_as_auto_land() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand"
confidence_threshold = 0.7"#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.9}"#)]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::AutoLand);
        assert_eq!(report.confidence, Some(0.9));
        assert_eq!(report.input_tokens, 100);
        assert_eq!(report.output_tokens, 25);
        // Mock provider returns Cost { total_cents: 3.0, .. } per call.
        assert_eq!(report.cost_cents, 3.0);

        let row = agent_runs_row(&sqlite, &report.run_id);
        assert_eq!(row.agent_name, "linker");
        assert!(row.completed_at.is_some());
        assert_eq!(row.outcome.as_deref(), Some("auto_land"));
        assert_eq!(row.input_tokens, Some(100));
        assert_eq!(row.output_tokens, Some(25));
        assert_eq!(row.cost_cents, Some(3.0));
        assert_eq!(row.errored, 0);
    }

    #[tokio::test]
    async fn low_confidence_response_results_in_no_action() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand"
confidence_threshold = 0.7"#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.4}"#)]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::NoAction);
        let row = agent_runs_row(&sqlite, &report.run_id);
        assert_eq!(row.outcome.as_deref(), Some("no_action"));
        assert_eq!(row.errored, 0);
    }

    #[tokio::test]
    async fn missing_confidence_field_is_no_action() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"value": 42}"#)]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::NoAction);
        assert_eq!(report.confidence, None);
    }

    #[tokio::test]
    async fn provider_error_marks_run_errored_and_persists_row() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Err("boom")]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::Errored);
        assert_eq!(report.cost_cents, 0.0);
        assert!(report.response_text.contains("boom"));
        let row = agent_runs_row(&sqlite, &report.run_id);
        assert!(
            row.completed_at.is_some(),
            "completed_at must be set even on provider error"
        );
        assert_eq!(row.outcome.as_deref(), Some("errored"));
        assert_eq!(
            row.errored, 1,
            "errored bool must be set for filter queries"
        );
        assert_eq!(row.cost_cents, Some(0.0));
        assert_eq!(row.input_tokens, Some(0));
        assert_eq!(row.output_tokens, Some(0));
    }

    #[tokio::test]
    async fn missing_agent_dir_errors_before_db_write() {
        let tmp = tempdir().unwrap();
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.9}"#)]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let err = runner
            .run_agent("nope", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap_err();
        assert!(matches!(err, RunnerError::AgentNotFound { .. }));

        // No agent_runs rows must be written when discovery fails.
        let count: i64 = {
            let conn = sqlite.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM agent_runs", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn malformed_config_errors_before_db_write() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            "this is not valid toml = = =",
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let err = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap_err();
        assert!(matches!(err, RunnerError::ConfigInvalid { .. }));

        let count: i64 = {
            let conn = sqlite.lock().unwrap();
            conn.query_row("SELECT COUNT(*) FROM agent_runs", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn file_change_trigger_records_note_in_notes_affected() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "file_change""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.95}"#)]));
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            provider,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let report = runner
            .run_agent(
                "linker",
                TriggerContext::FileChange {
                    note_id: "01HK000000000000000000000A".into(),
                },
            )
            .await
            .unwrap();

        let notes_affected: String = {
            let conn = sqlite.lock().unwrap();
            conn.query_row(
                "SELECT notes_affected FROM agent_runs WHERE id = ?1",
                rusqlite::params![report.run_id],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert!(notes_affected.contains("01HK000000000000000000000A"));
    }

    #[tokio::test]
    async fn correlation_id_is_distinct_from_run_id_and_propagated_to_prompt() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        // Use a provider that captures and echoes the rendered prompt
        // tail so we can assert the correlation_id flowed into the
        // dynamic-tail render.
        struct CapturingProvider {
            captured: StdMutex<Option<String>>,
        }
        #[async_trait]
        impl LlmProvider for CapturingProvider {
            async fn complete(
                &self,
                prompt: &PromptStructured,
                model: &Model,
                _options: &CompleteOptions,
            ) -> engram_llm::Result<Completion> {
                *self.captured.lock().unwrap() = Some(prompt.dynamic_tail.clone());
                Ok(Completion {
                    text: r#"{"confidence": 0.9}"#.to_string(),
                    usage: Usage::default(),
                    cost: Cost::unknown(),
                    model_used: model.name.clone(),
                    latency_ms: 0,
                })
            }
            async fn complete_streamed(
                &self,
                _: &PromptStructured,
                _: &Model,
                _: &CompleteOptions,
            ) -> engram_llm::Result<StreamedCompletion> {
                unreachable!()
            }
            async fn embed(&self, _: &str, _: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
                unreachable!()
            }
        }
        let provider = Arc::new(CapturingProvider {
            captured: StdMutex::new(None),
        });
        let runner = AgentRunner::new(
            Arc::clone(&sqlite),
            Arc::clone(&provider) as Arc<dyn LlmProvider>,
            test_model(),
            tmp.path().to_path_buf(),
        );

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_ne!(report.run_id, report.correlation_id);
        let tail = provider.captured.lock().unwrap().clone().unwrap();
        assert!(
            tail.contains(&report.correlation_id),
            "rendered tail must include correlation_id: {tail}"
        );
    }
}
