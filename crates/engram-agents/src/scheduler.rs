//! Agent scheduler: spawns long-lived dispatch tasks for the two
//! automated trigger kinds.
//!
//! [`AgentRunner::run_agent`](crate::runner::AgentRunner::run_agent)
//! handles a single on-demand invocation. The runner has three other
//! trigger kinds declared by config — `FileChange`, `Cron`, and
//! `CouncilOnly` — but only `OnDemand` had a producer. This module
//! adds the two automated producers:
//!
//! - **Cron** — one tokio task per `Cron`-trigger agent, looping on a
//!   `tokio::time::interval` whose period is the agent's
//!   [`crate::runner::AgentConfig::cron_interval_secs`]. On every
//!   tick the task calls `run_agent(name, TriggerContext::Cron)` and
//!   logs the outcome. Provider errors are caught and logged — the
//!   loop never propagates them, so a single failing run does not
//!   stop the schedule.
//!
//! - **FileChange** — one tokio task per `FileChange`-trigger agent,
//!   each owning its own [`tokio::sync::broadcast::Receiver`]
//!   subscribed from a shared sender owned by the caller. On every
//!   [`WatchEvent`] the task calls
//!   `run_agent(name, TriggerContext::FileChange { note_id })` and
//!   logs the outcome. Lagged receivers (slow agent + fast event
//!   stream) log a warning and resume; broadcast send-side overflow
//!   is the caller's concern.
//!
//! `CouncilOnly` is deliberately excluded — it depends on the council
//! deliberation engine (#34) which has no implementation yet.
//!
//! # Graceful shutdown
//!
//! [`SchedulerHandle::shutdown`] cancels the shared
//! `CancellationToken` every spawned task watches. Tasks observe the
//! cancellation at the next loop boundary (the next tick or the next
//! channel event) and exit cleanly — never mid-LLM-call. Use
//! [`SchedulerHandle::join_all`] to wait for every task to finish and
//! propagate any panic.
//!
//! # Per-agent concurrency
//!
//! Out of scope for this slice. The runner's [`crate::locks`] manager
//! already prevents concurrent writes to the same note, but two cron
//! ticks landing inside one agent's run_agent is allowed today (the
//! second observes the lock and returns `Deferred`). A future
//! `Semaphore`-based per-agent cap can be added on top without
//! changing the scheduler's API.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::runner::{AgentRunner, TriggerContext, TriggerKind};

/// Event published by an external file watcher and consumed by every
/// `FileChange`-trigger agent task spawned by [`AgentRunner::start_scheduler`].
///
/// The scheduler owns no file-watching code itself — that's the
/// integration boundary the caller fills in (using `notify`, an
/// editor plugin, or any other source). The scheduler just routes
/// the event into [`AgentRunner::run_agent`] with a
/// [`TriggerContext::FileChange`].
#[derive(Debug, Clone)]
pub struct WatchEvent {
    /// ULID of the note whose file changed. Surfaces to the agent's
    /// prompt via the `{{note_id}}` placeholder.
    pub note_id: String,
}

/// Handle to a running scheduler. Returned from
/// [`AgentRunner::start_scheduler`]. Drop or call [`Self::shutdown`]
/// to stop; [`Self::join_all`] to wait for every spawned task to
/// finish and propagate panics.
pub struct SchedulerHandle {
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl SchedulerHandle {
    /// Cancel every spawned task. Each task observes the cancellation
    /// at the next loop boundary (cron tick or channel event) and
    /// exits cleanly — never mid-LLM-call. Idempotent; safe to call
    /// from any task. Call [`Self::join_all`] afterward to wait for
    /// the tasks to actually finish.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    /// Await every spawned task. Returns once all tasks have exited.
    /// A task panic surfaces as a `JoinError` in the returned vector
    /// alongside the other tasks' `Ok(())` results — the caller can
    /// decide whether to bubble it. Consumes the handle so the
    /// cancellation token isn't dropped before the tasks observe it.
    pub async fn join_all(self) -> Vec<Result<(), tokio::task::JoinError>> {
        let mut out = Vec::with_capacity(self.tasks.len());
        for t in self.tasks {
            out.push(t.await);
        }
        out
    }

    /// Convenience: cancel and then await every task. Equivalent to
    /// calling [`Self::shutdown`] immediately followed by
    /// [`Self::join_all`].
    pub async fn shutdown_and_join(self) -> Vec<Result<(), tokio::task::JoinError>> {
        self.cancel.cancel();
        self.join_all().await
    }

    /// Number of tasks spawned. Useful in tests that assert the
    /// scheduler enumerated the expected set of agents.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

impl AgentRunner {
    /// Start the scheduler.
    ///
    /// Enumerates every configured agent (via
    /// [`Self::list_configured_agents`]) and spawns one task per
    /// `Cron`-trigger and per `FileChange`-trigger agent. `OnDemand`
    /// and `CouncilOnly` agents are skipped — they have no automated
    /// producer.
    ///
    /// `watch` is a sender owned by the caller; the scheduler calls
    /// `watch.subscribe()` per file-change agent. Pass a
    /// `broadcast::channel(_).0` even if no file watcher is wired up
    /// yet — the receivers will simply sit idle.
    ///
    /// Configuration-load failures during enumeration are logged via
    /// [`tracing::error!`] and the offending agent is skipped — one
    /// broken agent doesn't prevent the rest from running.
    pub fn start_scheduler(
        self: Arc<Self>,
        watch: broadcast::Sender<WatchEvent>,
    ) -> SchedulerHandle {
        let cancel = CancellationToken::new();
        let mut tasks = Vec::new();

        let names = match self.list_configured_agents() {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(error = %e, "scheduler: list_configured_agents failed; no tasks spawned");
                return SchedulerHandle { cancel, tasks };
            }
        };

        for name in names {
            // Pull the parsed config via the same hot-reload cache the
            // runner uses for actual invocations. Skip (and log) any
            // agent whose config fails to parse — one broken agent
            // shouldn't sink the whole scheduler.
            let trigger_and_period = match self.peek_trigger_and_period(&name) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        agent = %name,
                        error = %e,
                        "scheduler: failed to load agent config; skipping"
                    );
                    continue;
                }
            };
            match trigger_and_period {
                (TriggerKind::Cron, period_secs) => {
                    let runner = Arc::clone(&self);
                    let cancel = cancel.clone();
                    let agent_name = name.clone();
                    let period = Duration::from_secs(period_secs.max(1));
                    let handle = tokio::spawn(async move {
                        cron_loop(runner, agent_name, period, cancel).await;
                    });
                    tasks.push(handle);
                }
                (TriggerKind::FileChange, _) => {
                    let runner = Arc::clone(&self);
                    let cancel = cancel.clone();
                    let agent_name = name.clone();
                    let rx = watch.subscribe();
                    let handle = tokio::spawn(async move {
                        file_change_loop(runner, agent_name, rx, cancel).await;
                    });
                    tasks.push(handle);
                }
                (TriggerKind::OnDemand, _) | (TriggerKind::CouncilOnly, _) => {
                    // No automated producer for these trigger kinds.
                }
            }
        }

        SchedulerHandle { cancel, tasks }
    }
}

/// Cron dispatcher body. Loops on a [`tokio::time::interval`] whose
/// period equals the agent's `cron_interval_secs`. On every tick,
/// invokes `run_agent` and logs the outcome — never propagates the
/// error so a single failing run doesn't stop the schedule.
async fn cron_loop(
    runner: Arc<AgentRunner>,
    name: String,
    period: Duration,
    cancel: CancellationToken,
) {
    let mut tick = tokio::time::interval(period);
    // The first tick fires immediately by default; skip it so the
    // schedule starts after one period rather than racing the
    // caller's setup code.
    tick.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::info!(agent = %name, "scheduler: cron loop cancelled");
                break;
            }
            _ = tick.tick() => {
                match runner.run_agent(&name, TriggerContext::Cron).await {
                    Ok(report) => {
                        tracing::info!(
                            agent = %name,
                            outcome = ?report.outcome,
                            run_id = %report.run_id,
                            "scheduler: cron tick completed"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            agent = %name,
                            error = %e,
                            "scheduler: cron tick errored"
                        );
                    }
                }
            }
        }
    }
}

/// File-change dispatcher body. Receives [`WatchEvent`]s from the
/// per-task broadcast receiver and invokes `run_agent` with a
/// [`TriggerContext::FileChange`]. Lagged receivers log a warning
/// and resume; closed channels exit the loop.
async fn file_change_loop(
    runner: Arc<AgentRunner>,
    name: String,
    mut rx: broadcast::Receiver<WatchEvent>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                tracing::info!(agent = %name, "scheduler: file_change loop cancelled");
                break;
            }
            msg = rx.recv() => {
                match msg {
                    Ok(event) => {
                        let ctx = TriggerContext::FileChange { note_id: event.note_id.clone() };
                        match runner.run_agent(&name, ctx).await {
                            Ok(report) => {
                                tracing::info!(
                                    agent = %name,
                                    note_id = %event.note_id,
                                    outcome = ?report.outcome,
                                    run_id = %report.run_id,
                                    "scheduler: file_change dispatch completed"
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    agent = %name,
                                    note_id = %event.note_id,
                                    error = %e,
                                    "scheduler: file_change dispatch errored"
                                );
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            agent = %name,
                            skipped = n,
                            "scheduler: file_change receiver lagged; events dropped"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::info!(
                            agent = %name,
                            "scheduler: file_change channel closed; exiting"
                        );
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Scheduler tests use `tokio::time::pause` so cron intervals
    //! advance deterministically without sleeping. Each test spawns
    //! the scheduler, drives the clock or the channel, asserts on
    //! the database side effect (counting `agent_runs` rows is the
    //! cheapest probe), then shuts down cleanly.

    use super::*;
    use crate::locks::{LockConfig, LockManager};
    use crate::runner::AgentRunner;
    use async_trait::async_trait;
    use engram_index::sqlite::Migrator;
    use engram_llm::{
        CompleteOptions, Completion, Cost, EmbeddingModel, LlmProvider, Model, ModelProvider,
        PromptStructured, StreamedCompletion, Usage,
    };
    use rusqlite::Connection;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::sync::broadcast;

    /// Tiny provider that returns a canned, low-confidence JSON
    /// response and counts invocations. Scheduler tests don't care
    /// about response content — they only need `run_agent` to make
    /// it past the provider step so an `agent_runs` row lands.
    #[derive(Default)]
    struct CountingProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmProvider for CountingProvider {
        async fn complete(
            &self,
            _prompt: &PromptStructured,
            model: &Model,
            _options: &CompleteOptions,
        ) -> engram_llm::Result<Completion> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Completion {
                text: r#"{"confidence": 0.1}"#.to_string(),
                usage: Usage {
                    input_tokens_total: 1,
                    output_tokens: 1,
                    ..Default::default()
                },
                cost: Cost {
                    input_cents: 0.0,
                    cache_create_cents: 0.0,
                    cache_read_cents: 0.0,
                    output_cents: 0.0,
                    total_cents: 0.0,
                },
                model_used: format!("mock/{}", model.name),
                latency_ms: 0,
            })
        }

        async fn complete_streamed(
            &self,
            _prompt: &PromptStructured,
            _model: &Model,
            _options: &CompleteOptions,
        ) -> engram_llm::Result<StreamedCompletion> {
            unreachable!("scheduler tests never stream");
        }

        async fn embed(
            &self,
            _text: &str,
            _model: &EmbeddingModel,
        ) -> engram_llm::Result<Vec<f32>> {
            unreachable!("scheduler tests never embed");
        }
    }

    fn test_model() -> Model {
        Model {
            provider: ModelProvider::Anthropic,
            name: "test".into(),
        }
    }

    fn write_agent(root: &Path, name: &str, config: &str, prompt: &str) {
        let dir = root.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), config).unwrap();
        std::fs::write(dir.join("prompt.md"), prompt).unwrap();
    }

    fn setup_sqlite() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn insert_note(sqlite: &Arc<Mutex<Connection>>, id: &str) {
        let conn = sqlite.lock().unwrap();
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content) VALUES (?1, ?2, ?3, 'evergreen', '')",
            rusqlite::params![id, format!("{id}.md"), id],
        )
        .unwrap();
    }

    fn count_runs(sqlite: &Arc<Mutex<Connection>>, agent: &str) -> i64 {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE agent_name = ?1",
            rusqlite::params![agent],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn make_runner(
        sqlite: &Arc<Mutex<Connection>>,
        provider: Arc<dyn LlmProvider>,
        agents_root: &Path,
    ) -> Arc<AgentRunner> {
        let agents_dir = agents_root.join("agents");
        Arc::new(AgentRunner::new(
            Arc::clone(sqlite),
            provider,
            test_model(),
            agents_dir,
            LockManager::new(
                Arc::clone(sqlite),
                LockConfig {
                    ttl_secs: 60,
                    max_retries: 2,
                    retry_base_ms: 5,
                },
            ),
            agents_root.to_path_buf(),
        ))
    }

    // Cron: after N tick periods, agent_runs has N rows.
    #[tokio::test(start_paused = true)]
    async fn cron_loop_runs_once_per_interval() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "ticker",
            r#"name = "ticker"
trigger = "cron"
cron_interval_secs = 5
confidence_threshold = 0.99"#,
            "system\n<!-- /cache -->\nbody {{trigger}}\n",
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(CountingProvider::default());
        let runner = make_runner(&sqlite, provider, tmp.path());
        let (tx, _rx) = broadcast::channel::<WatchEvent>(8);

        let sched = runner.start_scheduler(tx);
        assert_eq!(sched.task_count(), 1, "exactly one cron task spawned");

        // Yield before the first advance so the spawned task starts
        // polling and creates its `interval` at t=0 — otherwise the
        // interval anchors at whatever time we've already advanced
        // to, and we lose one tick.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        // The first tick fires immediately (and is consumed); the
        // loop then awaits the next tick. We advance 5s three times
        // to get three real run_agent calls.
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(5)).await;
            // Yield generously so the spawned task observes the tick
            // and completes its full run_agent flow (which acquires
            // SQLite + calls the provider + writes the row) before
            // we measure. Paused-time sleep yields execution without
            // advancing the simulated clock.
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
        }

        sched.shutdown_and_join().await;
        assert_eq!(count_runs(&sqlite, "ticker"), 3);
    }

    // FileChange: an event with note_id "n1" dispatches to the
    // FileChange-trigger agent (and not to a Cron-trigger sibling).
    #[tokio::test(start_paused = true)]
    async fn file_change_dispatcher_routes_by_trigger_kind() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "watcher",
            r#"name = "watcher"
trigger = "file_change"
confidence_threshold = 0.99"#,
            "system\n<!-- /cache -->\nbody\n",
        );
        write_agent(
            tmp.path(),
            "ticker",
            r#"name = "ticker"
trigger = "cron"
cron_interval_secs = 3600
confidence_threshold = 0.99"#,
            "system\n<!-- /cache -->\nbody\n",
        );
        let sqlite = setup_sqlite();
        insert_note(&sqlite, "n1");
        let provider = Arc::new(CountingProvider::default());
        let runner = make_runner(&sqlite, provider, tmp.path());
        let (tx, _rx) = broadcast::channel::<WatchEvent>(8);

        let sched = runner.start_scheduler(tx.clone());
        assert_eq!(
            sched.task_count(),
            2,
            "one file_change + one cron task spawned"
        );

        // Allow the spawned subscribers to install their receivers
        // before publishing.
        tokio::task::yield_now().await;
        tx.send(WatchEvent {
            note_id: "n1".into(),
        })
        .unwrap();
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        sched.shutdown_and_join().await;

        assert_eq!(
            count_runs(&sqlite, "watcher"),
            1,
            "the file_change agent ran exactly once"
        );
        assert_eq!(
            count_runs(&sqlite, "ticker"),
            0,
            "the cron agent did not see the file_change event"
        );
    }

    // shutdown(): cancellation interrupts the cron loop at the next
    // boundary; no more runs land after shutdown.
    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_further_runs() {
        let tmp = tempdir().unwrap();
        write_agent(
            tmp.path(),
            "ticker",
            r#"name = "ticker"
trigger = "cron"
cron_interval_secs = 1
confidence_threshold = 0.99"#,
            "system\n<!-- /cache -->\nbody\n",
        );
        let sqlite = setup_sqlite();
        let provider = Arc::new(CountingProvider::default());
        let runner = make_runner(&sqlite, provider, tmp.path());
        let (tx, _rx) = broadcast::channel::<WatchEvent>(8);

        let sched = runner.start_scheduler(tx);

        // Yield once so the task starts polling and anchors its
        // interval at t=0 (otherwise the first advance is lost).
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        // Run two ticks.
        for _ in 0..2 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
        }
        let before = count_runs(&sqlite, "ticker");
        assert!(before >= 2, "expected at least 2 runs before shutdown");

        sched.shutdown_and_join().await;

        // Advance the clock further; no new run should land because
        // the task is gone.
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
        }
        let after = count_runs(&sqlite, "ticker");
        assert_eq!(after, before, "no runs land after shutdown");
    }

    // join_all(): a panicking task surfaces via the JoinError result.
    // We can't easily make run_agent panic, but we can spawn a task
    // directly into the handle by constructing a SchedulerHandle
    // manually — that's white-box but the contract is the only thing
    // worth asserting.
    #[tokio::test]
    async fn join_all_surfaces_panicking_task() {
        let cancel = CancellationToken::new();
        let panicking = tokio::spawn(async { panic!("intentional test panic") });
        let ok = tokio::spawn(async {});
        let handle = SchedulerHandle {
            cancel,
            tasks: vec![panicking, ok],
        };
        let results = handle.join_all().await;
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().any(|r| r.is_err()),
            "panicking task must surface as JoinError"
        );
    }

    // shutdown() before any tasks are spawned is a no-op (empty
    // scheduler).
    #[tokio::test]
    async fn empty_scheduler_shuts_down_cleanly() {
        let tmp = tempdir().unwrap();
        // No agents on disk.
        std::fs::create_dir_all(tmp.path().join("agents")).unwrap();
        let sqlite = setup_sqlite();
        let provider = Arc::new(CountingProvider::default());
        let runner = make_runner(&sqlite, provider, tmp.path());
        let (tx, _rx) = broadcast::channel::<WatchEvent>(8);

        let sched = runner.start_scheduler(tx);
        assert_eq!(sched.task_count(), 0);
        let results = sched.shutdown_and_join().await;
        assert!(results.is_empty());
    }
}
