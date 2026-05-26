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

use crate::invasiveness::{classify, DiffSummary, Invasiveness};
use crate::locks::{LockError, LockManager};
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
    /// Could not acquire the per-note lock (another agent holds it). The
    /// caller may retry later. No provider call was made and no cost was
    /// incurred. Surfaced to Pacekeeper in v2.1 per #27 implementation
    /// notes; today the agent layer can re-queue or drop on its own
    /// schedule.
    Deferred,
    /// The provider call panicked (an unwinding panic — usually
    /// `unreachable!` / `unwrap` in the provider or a wrapper). The
    /// runner catches the panic at the `JoinHandle` boundary so its own
    /// task stays healthy. Distinct from `Errored` so panic-rate metrics
    /// can be tracked separately from regular returned-Err API failures.
    Panicked,
}

impl RunOutcome {
    fn as_sql(&self) -> &'static str {
        match self {
            Self::AutoLand => "auto_land",
            Self::NoAction => "no_action",
            Self::CouncilConvened => "council_convened",
            Self::Errored => "errored",
            Self::Deferred => "deferred",
            Self::Panicked => "panicked",
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
    /// Invasiveness ceiling per `01-agents-and-council.md` §Invasiveness
    /// ceilings. When the response includes a `diff_summary`, the
    /// runner calls [`crate::invasiveness::classify`] and only allows
    /// [`RunOutcome::AutoLand`] if `verdict <= max_invasiveness`.
    /// Responses without a `diff_summary` skip this gate (back-compat
    /// with agents that don't yet structure their output that way).
    /// Default `Editorial` — moderate ceiling per the spec.
    #[serde(default = "default_max_invasiveness")]
    pub max_invasiveness: Invasiveness,
}

fn default_confidence_threshold() -> f32 {
    0.85
}

fn default_max_invasiveness() -> Invasiveness {
    Invasiveness::Editorial
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
    /// Invasiveness verdict produced by classifying the response's
    /// `diff_summary` field, if present. `None` when the response
    /// didn't include a `diff_summary` (the gate is then bypassed —
    /// see `AgentConfig::max_invasiveness` docs).
    pub invasiveness: Option<Invasiveness>,
    /// Action kind the agent self-classified (e.g. `link-add`,
    /// `tag-norm`, `note-create`). Parsed from the response's `kind`
    /// field. `None` when absent. Surfaces to the future agent_actions
    /// writer; today this is observable only via [`RunReport`] and the
    /// run's tracing span.
    pub kind: Option<String>,
    /// One-sentence rationale for the action, parsed from the
    /// response's `rationale` field. `None` when absent. Same surface
    /// as `kind`: observable today, consumed by the future
    /// `agent_actions` writer.
    pub rationale: Option<String>,
    /// Raw response text (kept so callers can route to action-log /
    /// proposal-filer in follow-up slices).
    pub response_text: String,
}

/// One cached agent's parsed config + prompt plus the mtimes the
/// cache was built from. The runner consults the cache on every
/// `run_agent` call and reloads when either file's mtime has moved.
///
/// Cloning is cheap (an `Arc` bump) so in-flight runs hold their own
/// reference for the duration of the call — a concurrent reload
/// replaces the cache slot's `Arc` but doesn't disturb the in-flight
/// run's view. This matches the AC's "in-flight runs continue with the
/// prompt loaded at start time" semantics.
struct CachedAgent {
    config: AgentConfig,
    prompt: PromptStructured,
    config_mtime: std::time::SystemTime,
    prompt_mtime: std::time::SystemTime,
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
    locks: LockManager,
    /// Per-agent cache of parsed config + prompt, invalidated by file
    /// mtime change. See [`CachedAgent`].
    cache: Mutex<std::collections::HashMap<String, Arc<CachedAgent>>>,
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
    /// - `locks` — per-note advisory lock manager; the runner acquires a
    ///   lock for any trigger that names a note (`OnDemand` with `note_id`,
    ///   `FileChange`) and holds it across the provider call. On
    ///   acquisition failure the run returns [`RunOutcome::Deferred`]
    ///   without contacting the provider
    pub fn new(
        sqlite: Arc<Mutex<Connection>>,
        provider: Arc<dyn LlmProvider>,
        model: Model,
        agents_dir: PathBuf,
        locks: LockManager,
    ) -> Self {
        Self {
            sqlite,
            provider,
            model,
            agents_dir,
            locks,
            cache: Mutex::new(std::collections::HashMap::new()),
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

        // `kind` and `rationale` are recorded later via
        // `Span::record` once we've parsed the response. Declared
        // here as `tracing::field::Empty` so subscribers see the
        // field names even before the values land.
        let span = info_span!(
            "agent_run",
            agent = name,
            correlation_id = %correlation_id,
            run_id = %run_id,
            trigger = trigger.trigger_label(),
            kind = tracing::field::Empty,
            rationale = tracing::field::Empty,
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
        // open-ended row. The cache returns an `Arc` so a concurrent
        // reload won't disturb this in-flight run.
        let cached = self.load_cached(name)?;
        let config = &cached.config;
        let prompt = &cached.prompt;

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

        // Acquire the per-note advisory lock for triggers that name a
        // note. `LockManager::acquire` is sync (std::thread::sleep in its
        // jittered backoff); run it on a blocking task so we don't stall
        // the async executor when contended. On AcquisitionFailed we mark
        // the run Deferred without contacting the provider — no tokens
        // burned, no cost incurred.
        //
        // The guard binds the lock to this task; the RAII Drop releases
        // it when the function returns (including panic unwind). Drop the
        // guard explicitly after the provider call so the borrow is clear
        // to future maintainers, even though it would drop at scope end.
        let _lock_guard = match trigger.note_id() {
            Some(note_id) => match self.try_acquire_note_lock(note_id, name).await {
                Ok(guard) => Some(guard),
                Err(reason) => {
                    return self
                        .finalize_deferred(&run_id, &correlation_id, name, reason)
                        .await;
                }
            },
            None => None,
        };

        // Render the dynamic tail with the trigger's context. Today this
        // is a minimal `{{trigger}}` / `{{note_id}}` substitution; future
        // slices will assemble neighbors, biographer model, etc.
        let rendered_tail = render_dynamic_tail(prompt, &trigger, &correlation_id);

        let final_prompt = PromptStructured {
            static_head: prompt.static_head.clone(),
            dynamic_tail: rendered_tail,
        };

        // Call the provider. Spawn the call as its own task so panics
        // (typically `unreachable!`/`unwrap` bugs in a provider wrapper)
        // surface as `JoinError::is_panic()` instead of unwinding through
        // the runner's task. The agent_runs row is already INSERTed and
        // will be UPDATEd below regardless, so a panic still produces a
        // complete lifecycle trail.
        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        let options = CompleteOptions::default();
        let join_handle =
            tokio::spawn(async move { provider.complete(&final_prompt, &model, &options).await });
        let join_result = join_handle.await;

        let (
            outcome,
            input_tokens,
            output_tokens,
            cost_cents,
            confidence,
            invasiveness,
            kind,
            rationale,
            response_text,
        ) = match join_result {
            Ok(Ok(completion)) => {
                let confidence = parse_confidence(&completion.text);
                let invasiveness = parse_diff_summary(&completion.text).map(|d| classify(&d));
                let kind = parse_string_field(&completion.text, "kind");
                let rationale = parse_string_field(&completion.text, "rationale");
                // Auto-land requires BOTH gates to pass:
                //   confidence ≥ threshold  AND  invasiveness ≤ max
                // If the response didn't include a diff_summary, only
                // the confidence gate applies — back-compat for agents
                // whose output schema doesn't yet structure the diff.
                let confidence_ok = confidence.is_some_and(|c| c >= config.confidence_threshold);
                let invasiveness_ok = invasiveness.is_none_or(|v| v <= config.max_invasiveness);
                let outcome = if confidence_ok && invasiveness_ok {
                    RunOutcome::AutoLand
                } else {
                    RunOutcome::NoAction
                };
                // Enrich the run's tracing span with what we now know
                // about the agent's intent. Span fields are filtered
                // out cleanly when subscribers don't want them.
                tracing::Span::current().record("kind", kind.as_deref().unwrap_or(""));
                tracing::Span::current().record("rationale", rationale.as_deref().unwrap_or(""));
                (
                    outcome,
                    completion.usage.input_tokens_total,
                    completion.usage.output_tokens,
                    completion.cost.total_cents,
                    confidence,
                    invasiveness,
                    kind,
                    rationale,
                    completion.text,
                )
            }
            Ok(Err(e)) => {
                // Provider returned an error — record as Errored and
                // surface the message in the response slot so the
                // agent_runs row is a complete trail.
                (
                    RunOutcome::Errored,
                    0,
                    0,
                    0.0,
                    None,
                    None,
                    None,
                    None,
                    format!("provider error: {e}"),
                )
            }
            Err(join_err) if join_err.is_panic() => {
                // Provider task panicked — best-effort extract the
                // payload's string for diagnostics. The runner's own
                // task remains healthy.
                let payload = join_err.into_panic();
                let msg = panic_payload_to_string(payload);
                (RunOutcome::Panicked, 0, 0, 0.0, None, None, None, None, msg)
            }
            Err(join_err) => {
                // Cancellation — `tokio::spawn` futures are not
                // cancelled by the runner; if this ever fires it's a
                // runtime-shutdown signal. Treat as Errored.
                (
                    RunOutcome::Errored,
                    0,
                    0,
                    0.0,
                    None,
                    None,
                    None,
                    None,
                    format!("provider task cancelled: {join_err}"),
                )
            }
        };

        // Both `Errored` (provider-returned Err) and `Panicked` (provider
        // unwinding panic) count as failures for the boolean flag. `Deferred`
        // is a resource conflict, not a failure.
        let errored = matches!(outcome, RunOutcome::Errored | RunOutcome::Panicked) as i64;
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
            invasiveness,
            kind,
            rationale,
            response_text,
        })
    }

    /// Attempt to acquire the per-note advisory lock. `LockManager::acquire`
    /// is synchronous (it does its own jittered-backoff retries via
    /// `std::thread::sleep`), so we wrap the call in `spawn_blocking` to
    /// keep the async executor responsive when there's contention.
    async fn try_acquire_note_lock(
        &self,
        note_id: &str,
        holder: &str,
    ) -> Result<crate::locks::LockGuard, String> {
        let locks = self.locks.clone();
        let note_id_owned = note_id.to_string();
        let holder_owned = holder.to_string();
        let result =
            tokio::task::spawn_blocking(move || locks.acquire(&note_id_owned, &holder_owned, None))
                .await
                .map_err(|join_err| format!("lock-acquire task panicked: {join_err}"))?;
        result.map_err(|e| match e {
            LockError::AcquisitionFailed {
                current_holder,
                expires_at,
                ..
            } => format!("lock held by `{current_holder}` until {expires_at}; deferred"),
            LockError::Sqlite(msg) => format!("lock sqlite error: {msg}"),
        })
    }

    /// Write the terminal `agent_runs` row for a deferred run (no
    /// provider call made) and return the `RunReport`.
    async fn finalize_deferred(
        &self,
        run_id: &str,
        correlation_id: &str,
        name: &str,
        reason: String,
    ) -> Result<RunReport, RunnerError> {
        let completed_at = Utc::now().to_rfc3339();
        {
            let conn = self.sqlite.lock().expect("sqlite mutex poisoned");
            conn.execute(
                "UPDATE agent_runs SET completed_at = ?1, outcome = ?2, \
                 input_tokens = ?3, output_tokens = ?4, cost_cents = ?5, errored = ?6 \
                 WHERE id = ?7",
                rusqlite::params![
                    completed_at,
                    RunOutcome::Deferred.as_sql(),
                    0_i64,
                    0_i64,
                    0.0_f64,
                    0_i64,
                    run_id,
                ],
            )?;
        }
        Ok(RunReport {
            run_id: run_id.to_string(),
            correlation_id: correlation_id.to_string(),
            agent: name.to_string(),
            outcome: RunOutcome::Deferred,
            input_tokens: 0,
            output_tokens: 0,
            cost_cents: 0.0,
            confidence: None,
            invasiveness: None,
            kind: None,
            rationale: None,
            response_text: reason,
        })
    }

    /// Return a cached [`CachedAgent`] for `name`, reloading from disk
    /// only when either `config.toml` or `prompt.md` has changed mtime
    /// since the last load. First call always reads.
    ///
    /// Concurrency: the cache slot is `Arc<CachedAgent>`. A second
    /// concurrent call that observes a stale mtime will re-read and
    /// replace the slot; the first caller's `Arc` clone is unaffected
    /// (RAII keeps the prior load alive for the duration of the
    /// in-flight run).
    fn load_cached(&self, name: &str) -> Result<Arc<CachedAgent>, RunnerError> {
        let config_path = self.agents_dir.join(name).join("config.toml");
        let prompt_path = self.agents_dir.join(name).join("prompt.md");

        // Stat both files first. If either mtime read fails, propagate
        // the AgentNotFound error so the caller surfaces a useful
        // message about which file is missing.
        let config_mtime = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .map_err(|e| RunnerError::AgentNotFound {
                name: name.to_string(),
                detail: format!("config.toml at {}: {}", config_path.display(), e),
            })?;
        let prompt_mtime = std::fs::metadata(&prompt_path)
            .and_then(|m| m.modified())
            .map_err(|e| RunnerError::AgentNotFound {
                name: name.to_string(),
                detail: format!("prompt.md at {}: {}", prompt_path.display(), e),
            })?;

        // Fast path: cache hit with matching mtimes.
        {
            let cache = self.cache.lock().expect("cache mutex poisoned");
            if let Some(entry) = cache.get(name) {
                if entry.config_mtime == config_mtime && entry.prompt_mtime == prompt_mtime {
                    return Ok(Arc::clone(entry));
                }
            }
        }

        // Slow path: stale or first-time. Read + parse + replace.
        let raw =
            std::fs::read_to_string(&config_path).map_err(|e| RunnerError::AgentNotFound {
                name: name.to_string(),
                detail: format!("config.toml at {}: {}", config_path.display(), e),
            })?;
        let config = AgentConfig::from_toml(&raw).map_err(|source| RunnerError::ConfigInvalid {
            name: name.to_string(),
            source,
        })?;
        let prompt =
            prompt_loader::load(&prompt_path).map_err(|source| RunnerError::PromptInvalid {
                name: name.to_string(),
                source,
            })?;
        let fresh = Arc::new(CachedAgent {
            config,
            prompt,
            config_mtime,
            prompt_mtime,
        });
        {
            let mut cache = self.cache.lock().expect("cache mutex poisoned");
            cache.insert(name.to_string(), Arc::clone(&fresh));
        }
        Ok(fresh)
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

/// Best-effort stringify of a tokio task panic payload. The payload is
/// `Box<dyn Any + Send>`; the standard library convention is that
/// `panic!(...)`-produced payloads are either `&'static str` or `String`,
/// so we downcast to both. Unknown payload types fall back to a generic
/// message so the agent_runs row always has something useful to read.
fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        format!("provider panicked: {s}")
    } else if let Some(s) = payload.downcast_ref::<String>() {
        format!("provider panicked: {s}")
    } else {
        "provider panicked (non-string payload)".to_string()
    }
}

/// Best-effort extraction of `diff_summary` (a serialized
/// [`DiffSummary`]) from a JSON response body. Non-JSON, missing-field,
/// and parse-error cases return `None` so the invasiveness gate is
/// bypassed (back-compat with agents that don't yet structure their
/// output that way).
fn parse_diff_summary(text: &str) -> Option<DiffSummary> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let field = v.get("diff_summary")?;
    serde_json::from_value(field.clone()).ok()
}

/// Best-effort extraction of a top-level string field from a JSON
/// response body. Non-JSON, missing-field, or non-string-value cases
/// return `None`. Used for `kind` and `rationale` parsing.
fn parse_string_field(text: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get(field).and_then(|f| f.as_str()).map(String::from)
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

    /// Default LockManager for tests — fast retries so the contended-lock
    /// test resolves in tens of milliseconds rather than the production
    /// default's 300+ ms.
    fn test_locks(sqlite: &Arc<Mutex<Connection>>) -> LockManager {
        LockManager::new(
            Arc::clone(sqlite),
            crate::locks::LockConfig {
                ttl_secs: 60,
                max_retries: 2,
                retry_base_ms: 5,
            },
        )
    }

    /// Insert a minimal `notes` row so the `note_locks` foreign-key
    /// constraint is satisfied for lock-acquisition tests.
    fn insert_note_stub(sqlite: &Arc<Mutex<Connection>>, note_id: &str) {
        let conn = sqlite.lock().unwrap();
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content) \
             VALUES (?1, ?2, ?3, 'evergreen', '')",
            rusqlite::params![note_id, format!("{note_id}.md"), note_id],
        )
        .unwrap();
    }

    fn make_runner(
        sqlite: &Arc<Mutex<Connection>>,
        provider: Arc<dyn LlmProvider>,
        agents_dir: &Path,
    ) -> AgentRunner {
        AgentRunner::new(
            Arc::clone(sqlite),
            provider,
            test_model(),
            agents_dir.to_path_buf(),
            test_locks(sqlite),
        )
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
        let runner = make_runner(&sqlite, provider, tmp.path());

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
        let runner = make_runner(&sqlite, provider, tmp.path());

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
        let runner = make_runner(&sqlite, provider, tmp.path());

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
        let runner = make_runner(&sqlite, provider, tmp.path());

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

    /// When two runs target the same note ID and arrive close enough that the
    /// LockManager's backoff retries don't outlast the first holder, the
    /// second run resolves as Deferred — no provider call made, cost zero,
    /// `agent_runs.outcome = 'deferred'`.
    #[tokio::test]
    async fn concurrent_runs_on_same_note_one_is_deferred() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "file_change""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        insert_note_stub(&sqlite, "01HK00000000000000000000LL");

        // Manually acquire the lock first to guarantee contention — we
        // don't want test-timing to decide which of two racing runs wins.
        let locks = test_locks(&sqlite);
        let _held = locks
            .acquire("01HK00000000000000000000LL", "other-holder", None)
            .expect("pre-acquire");

        // Now invoke the runner against that note. Both `OnDemand` with a
        // note_id and `FileChange` should defer; exercise both.
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.9}"#)]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent(
                "linker",
                TriggerContext::FileChange {
                    note_id: "01HK00000000000000000000LL".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::Deferred);
        assert_eq!(report.cost_cents, 0.0);
        assert_eq!(report.input_tokens, 0);
        assert_eq!(report.output_tokens, 0);
        assert!(report.response_text.contains("held by `other-holder`"));

        let row = agent_runs_row(&sqlite, &report.run_id);
        assert_eq!(row.outcome.as_deref(), Some("deferred"));
        assert_eq!(row.errored, 0, "deferred is not an error");
        assert!(row.completed_at.is_some());
    }

    /// Triggers with no note_id (Cron, Council, OnDemand{None}) bypass lock
    /// acquisition entirely — no contention possible because there's no
    /// note to lock.
    #[tokio::test]
    async fn cron_trigger_does_not_attempt_lock() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "cron""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        insert_note_stub(&sqlite, "01HK00000000000000000000UN");
        // Pre-hold a lock on an unrelated note — the cron-triggered run
        // should ignore it and proceed.
        let _held = test_locks(&sqlite)
            .acquire("01HK00000000000000000000UN", "other-holder", None)
            .expect("pre-acquire");
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.95}"#)]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::Cron)
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::AutoLand);
        assert_eq!(report.input_tokens, 100);
    }

    /// After a successful run releases the note lock (RAII Drop), a
    /// follow-up run on the same note acquires cleanly.
    #[tokio::test]
    async fn lock_releases_after_run_so_next_run_succeeds() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "file_change""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        insert_note_stub(&sqlite, "01HK00000000000000000000RL");
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(r#"{"confidence": 0.95}"#),
            Ok(r#"{"confidence": 0.95}"#),
        ]));
        let runner = make_runner(&sqlite, provider, tmp.path());
        let trigger = || TriggerContext::FileChange {
            note_id: "01HK00000000000000000000RL".to_string(),
        };

        let first = runner.run_agent("linker", trigger()).await.unwrap();
        assert_eq!(first.outcome, RunOutcome::AutoLand);

        let second = runner.run_agent("linker", trigger()).await.unwrap();
        assert_eq!(
            second.outcome,
            RunOutcome::AutoLand,
            "second run on same note must succeed after first guard drops"
        );
    }

    /// A provider that panics inside `complete` resolves the run as
    /// Panicked (distinct from Errored) and the agent_runs row is fully
    /// populated. The runner's own task remains usable for follow-up
    /// invocations.
    #[tokio::test]
    async fn provider_panic_is_caught_and_marked_panicked() {
        struct PanickingProvider;
        #[async_trait]
        impl LlmProvider for PanickingProvider {
            async fn complete(
                &self,
                _prompt: &PromptStructured,
                _model: &Model,
                _options: &CompleteOptions,
            ) -> engram_llm::Result<Completion> {
                panic!("simulated provider bug");
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

        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let runner = make_runner(&sqlite, Arc::new(PanickingProvider), tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.outcome, RunOutcome::Panicked);
        assert_eq!(report.input_tokens, 0);
        assert_eq!(report.output_tokens, 0);
        assert_eq!(report.cost_cents, 0.0);
        assert!(
            report.response_text.contains("simulated provider bug"),
            "panic payload should surface in response_text: {}",
            report.response_text
        );

        let row = agent_runs_row(&sqlite, &report.run_id);
        assert!(row.completed_at.is_some());
        assert_eq!(row.outcome.as_deref(), Some("panicked"));
        assert_eq!(
            row.errored, 1,
            "panic counts as a failure for the errored flag"
        );

        // Runner is still usable after a prior panic — second call goes
        // through cleanly with a normal provider.
        let provider2 = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.9}"#)]));
        let runner2 = make_runner(&sqlite, provider2, tmp.path());
        let r2 = runner2
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        assert_eq!(r2.outcome, RunOutcome::AutoLand);
    }

    #[test]
    fn panic_payload_to_string_handles_static_str_string_and_other() {
        let s = panic_payload_to_string(Box::new("static str panic"));
        assert!(s.contains("static str panic"));
        let s = panic_payload_to_string(Box::new("owned String panic".to_string()));
        assert!(s.contains("owned String panic"));
        let s = panic_payload_to_string(Box::new(42_u64));
        assert!(s.contains("non-string payload"));
    }

    /// High confidence + Structural verdict → NoAction (the
    /// invasiveness gate blocks auto-land even at high confidence).
    /// Mirrors the spec table: file create/delete always requires
    /// human approval, never auto-lands.
    #[tokio::test]
    async fn structural_verdict_blocks_autoland_at_high_confidence() {
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
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(
            r#"{"confidence": 0.95, "diff_summary": {"creates_or_deletes_files": true}}"#,
        )]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(
            report.invasiveness,
            Some(Invasiveness::Structural),
            "diff classified as Structural"
        );
        assert_eq!(
            report.outcome,
            RunOutcome::NoAction,
            "high confidence ALONE is not enough — Structural always blocks"
        );
    }

    /// Mechanical verdict + high confidence → AutoLand. Both gates pass.
    #[tokio::test]
    async fn mechanical_verdict_lands_at_high_confidence() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "gardener",
            r#"name = "gardener"
trigger = "on_demand"
confidence_threshold = 0.7"#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(
            r#"{"confidence": 0.95, "diff_summary": {"is_pure_metadata_normalization": true}}"#,
        )]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("gardener", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.invasiveness, Some(Invasiveness::Mechanical));
        assert_eq!(report.outcome, RunOutcome::AutoLand);
    }

    /// A response with no `diff_summary` field bypasses the
    /// invasiveness gate entirely — back-compat for agents whose output
    /// schema doesn't yet structure the diff. Only the confidence gate
    /// applies.
    #[tokio::test]
    async fn missing_diff_summary_bypasses_invasiveness_gate() {
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
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.95}"#)]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.invasiveness, None);
        assert_eq!(report.outcome, RunOutcome::AutoLand);
    }

    /// `max_invasiveness = mechanical` ceiling: even an Additive
    /// verdict blocks AutoLand. Documents the per-agent gate.
    #[tokio::test]
    async fn per_agent_ceiling_blocks_above_threshold_verdict() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "witness",
            r#"name = "witness"
trigger = "on_demand"
confidence_threshold = 0.7
max_invasiveness = "mechanical""#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        // Additive diff: new safe-kind block only.
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(
            r#"{"confidence": 0.95, "diff_summary": {"adds_new_blocks_only": true, "additive_only_safe_kinds": true}}"#,
        )]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("witness", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.invasiveness, Some(Invasiveness::Additive));
        assert_eq!(
            report.outcome,
            RunOutcome::NoAction,
            "Additive > Mechanical ceiling; auto-land blocked"
        );
    }

    /// Malformed `diff_summary` (wrong shape) gracefully falls through
    /// to "no verdict" rather than failing the run.
    #[tokio::test]
    async fn malformed_diff_summary_bypasses_gate_gracefully() {
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
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(
            r#"{"confidence": 0.95, "diff_summary": "not an object"}"#,
        )]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(
            report.invasiveness, None,
            "unparseable summary → no verdict"
        );
        // Confidence gate still applies (and passes here), so AutoLand.
        assert_eq!(report.outcome, RunOutcome::AutoLand);
    }

    /// First call to `run_agent` reads from disk; second call within
    /// the same runner uses the cache, with no disk re-read or re-parse.
    /// Verified by an instrumented filesystem: we record file reads via
    /// a `read_count` test helper that wraps `fs::read_to_string` — but
    /// since we can't intercept stdlib calls, the test instead asserts
    /// the cache slot was populated and that mutating the *cached*
    /// `AgentConfig` (via `runner.cache` test peek) is what the run
    /// sees on the second call (proves the cache is what's consulted).
    #[tokio::test]
    async fn second_call_uses_cache() {
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
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(r#"{"confidence": 0.9}"#),
            Ok(r#"{"confidence": 0.9}"#),
        ]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        // First call populates the cache.
        let _ = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        let cached_after_first = runner.cache.lock().unwrap().get("linker").cloned().unwrap();

        // Overwrite the config.toml on disk but *keep its mtime* (set
        // it back to what we recorded). The cache should not notice and
        // should serve the original config on the second call.
        let original_mtime = cached_after_first.config_mtime;
        let cfg_path = tmp.path().join("linker").join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"name = "rewritten"
trigger = "on_demand"
confidence_threshold = 0.99"#,
        )
        .unwrap();
        // Reset the file's mtime to the original. On macOS / Linux,
        // `filetime` is the standard way, but to avoid the extra dep
        // we use `utimes` via libc through the std `set_modified` API
        // (stable since Rust 1.75).
        std::fs::File::open(&cfg_path)
            .unwrap()
            .set_modified(original_mtime)
            .unwrap();

        // Second call should use the cached config (confidence_threshold = 0.7),
        // so a 0.9-confidence response still AutoLands. If we were re-reading,
        // the rewritten config (threshold = 0.99) would resolve to NoAction.
        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        assert_eq!(
            report.outcome,
            RunOutcome::AutoLand,
            "cached config (threshold 0.7) should still apply; on-disk rewrite is invisible"
        );

        // Sanity: the cache slot's pointer is the same as the one we
        // captured after the first call (the Arc was reused, not
        // replaced).
        let cached_after_second = runner.cache.lock().unwrap().get("linker").cloned().unwrap();
        assert!(
            Arc::ptr_eq(&cached_after_first, &cached_after_second),
            "cache slot's Arc must be identical when mtimes match"
        );
    }

    /// Touching the config file (advancing mtime) triggers a reload.
    /// Verified by overwriting with a *narrower* threshold that flips
    /// outcome from AutoLand to NoAction.
    #[tokio::test]
    async fn mtime_change_triggers_reload() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand"
confidence_threshold = 0.5"#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(r#"{"confidence": 0.7}"#),
            Ok(r#"{"confidence": 0.7}"#),
        ]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let first = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        assert_eq!(first.outcome, RunOutcome::AutoLand);

        // Rewrite the config with a higher threshold *and* advance the
        // mtime past the cache's recorded value. `set_modified` to
        // `now + 1s` guarantees a forward jump on every filesystem.
        let bump = std::time::SystemTime::now() + std::time::Duration::from_secs(1);
        let cfg_path = tmp.path().join("linker").join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"name = "linker"
trigger = "on_demand"
confidence_threshold = 0.9"#,
        )
        .unwrap();
        std::fs::File::open(&cfg_path)
            .unwrap()
            .set_modified(bump)
            .unwrap();

        // The second call should re-read; with the new threshold of
        // 0.9, a 0.7-confidence response resolves to NoAction.
        let second = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        assert_eq!(
            second.outcome,
            RunOutcome::NoAction,
            "mtime bump must trigger reload + new threshold"
        );
    }

    /// Two agents have independent cache slots — reloading one doesn't
    /// invalidate the other.
    #[tokio::test]
    async fn per_agent_cache_isolation() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "linker",
            r#"name = "linker"
trigger = "on_demand"
confidence_threshold = 0.5"#,
            DEMO_PROMPT,
        );
        write_agent(
            tmp.path(),
            "gardener",
            r#"name = "gardener"
trigger = "on_demand"
confidence_threshold = 0.5"#,
            DEMO_PROMPT,
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![
            Ok(r#"{"confidence": 0.9}"#),
            Ok(r#"{"confidence": 0.9}"#),
            Ok(r#"{"confidence": 0.9}"#),
        ]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        // Prime both caches.
        let _ = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        let gardener_first = runner.cache.lock().unwrap().get("gardener").cloned();
        let _ = runner
            .run_agent("gardener", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        let linker_arc = runner.cache.lock().unwrap().get("linker").cloned().unwrap();
        let gardener_arc = runner
            .cache
            .lock()
            .unwrap()
            .get("gardener")
            .cloned()
            .unwrap();

        // Mutate linker's config + bump mtime; gardener untouched.
        let bump = std::time::SystemTime::now() + std::time::Duration::from_secs(1);
        let linker_cfg = tmp.path().join("linker").join("config.toml");
        std::fs::write(
            &linker_cfg,
            r#"name = "linker"
trigger = "on_demand"
confidence_threshold = 0.99"#,
        )
        .unwrap();
        std::fs::File::open(&linker_cfg)
            .unwrap()
            .set_modified(bump)
            .unwrap();

        // Trigger linker reload.
        let _ = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();
        let linker_arc_after = runner.cache.lock().unwrap().get("linker").cloned().unwrap();
        let gardener_arc_after = runner
            .cache
            .lock()
            .unwrap()
            .get("gardener")
            .cloned()
            .unwrap();

        assert!(
            !Arc::ptr_eq(&linker_arc, &linker_arc_after),
            "linker's cache slot should have been replaced"
        );
        assert!(
            Arc::ptr_eq(&gardener_arc, &gardener_arc_after),
            "gardener's cache slot must NOT be disturbed by linker's reload"
        );
        // Drop the unused first-snapshot to avoid an unused warning.
        let _ = gardener_first;
    }

    /// `kind` and `rationale` flow through from the response into
    /// `RunReport`. They're optional — agents that don't emit them
    /// still produce valid runs.
    #[tokio::test]
    async fn kind_and_rationale_propagate_to_report() {
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
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{
                "confidence": 0.9,
                "kind": "link-add",
                "rationale": "Added missing wikilink to the source note's tagged concept."
            }"#)]));
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert_eq!(report.kind.as_deref(), Some("link-add"));
        assert_eq!(
            report.rationale.as_deref(),
            Some("Added missing wikilink to the source note's tagged concept.")
        );
    }

    /// A response without `kind` or `rationale` produces `None` on
    /// both — the runner doesn't require them.
    #[tokio::test]
    async fn missing_kind_and_rationale_are_none() {
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
        let runner = make_runner(&sqlite, provider, tmp.path());

        let report = runner
            .run_agent("linker", TriggerContext::OnDemand { note_id: None })
            .await
            .unwrap();

        assert!(report.kind.is_none());
        assert!(report.rationale.is_none());
    }

    /// Non-string values in the `kind` / `rationale` slots are
    /// ignored — only string values populate the field.
    #[test]
    fn parse_string_field_only_accepts_strings() {
        assert_eq!(
            parse_string_field(r#"{"kind": "link-add"}"#, "kind"),
            Some("link-add".to_string())
        );
        // Number value in `kind`: ignored.
        assert_eq!(parse_string_field(r#"{"kind": 42}"#, "kind"), None);
        // Missing key.
        assert_eq!(parse_string_field(r#"{"confidence": 0.9}"#, "kind"), None);
        // Non-JSON body.
        assert_eq!(parse_string_field("not json", "kind"), None);
    }

    #[tokio::test]
    async fn missing_agent_dir_errors_before_db_write() {
        let tmp = tempdir().unwrap();
        let sqlite = setup_sqlite();
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.9}"#)]));
        let runner = make_runner(&sqlite, provider, tmp.path());

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
        let runner = make_runner(&sqlite, provider, tmp.path());

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
        insert_note_stub(&sqlite, "01HK000000000000000000000A");
        let provider = Arc::new(ScriptedProvider::new(vec![Ok(r#"{"confidence": 0.95}"#)]));
        let runner = make_runner(&sqlite, provider, tmp.path());

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
        let runner = make_runner(
            &sqlite,
            Arc::clone(&provider) as Arc<dyn LlmProvider>,
            tmp.path(),
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
