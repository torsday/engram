//! Agent host: scheduler, runner, council deliberation, and review queue.

use thiserror::Error;

/// Errors produced by the backup watcher.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BackupError {
    /// An external probe command (git, tmutil) failed.
    #[error("probe failed: {0}")]
    Probe(String),
    /// Writing the status file failed.
    #[error("write failed: {0}")]
    Write(String),
}

/// Backup Watcher meta-agent: monitors git remote, filesystem snapshots, and
/// artifact remote recency without performing any backup operations.
pub mod backup_watcher;

/// Per-note advisory lock manager (prevents concurrent agent writes to the same note).
pub mod locks;

/// Agent action log: records every unstaged agent write and reconciles human
/// decisions (`staged`, `rejected`, `amended`) when `.git/index` changes.
pub mod action_log;
/// Agent identity, configuration, and lifecycle (ready → running → done).
pub mod identity {}

/// Agent scheduler: spawns one tokio task per `Cron`-trigger agent
/// (interval-based) and one per `FileChange`-trigger agent
/// (broadcast-subscribed). Graceful shutdown via `CancellationToken`.
/// See [`scheduler::SchedulerHandle`].
pub mod scheduler;

/// Agent runner: on-demand invocation, `agent_runs` row writing, correlation
/// IDs. Scheduler / file-change dispatch / cron loop land in follow-ups.
pub mod runner;

/// Bridge between [`runner::AgentRunner`] and the [`engram_eval`]
/// crate's `Invoker` contract. Exposes the production adapter that
/// the eval framework's CLI / runner can use to invoke a real
/// agent against a seeded vault.
pub mod eval_adapter;

/// Invasiveness classifier — deterministic, no-LLM verdict over a diff
/// summary per `01-agents-and-council.md` §Invasiveness classifier.
/// Consumed by the decision-matrix slice (`confidence × invasiveness`
/// auto-land gate).
pub mod invasiveness;

/// Conservative text-level walker that produces a `DiffSummary` from a
/// before/after content pair. Handles the easy cases (identical
/// content, pure whitespace normalization, pure line insertion);
/// everything else falls through to `modifies_existing_text_blocks`
/// → Editorial (safe). The full markdown-AST walker (link removal,
/// frontmatter critical-field detection, additive-kind safety) is a
/// separate slice.
pub mod diff_walker;

/// Markdown-AST `DiffSummary` walker — AST-aware producer that adds
/// link-removal, critical-frontmatter, and safe-additive-kind
/// signals on top of the text walker. Drop-in replacement for
/// `summarize_text_diff` when AST-level analysis is wanted.
pub mod ast_walker;

/// Prompt loader — splits `agents/<name>/prompt.md` on the cache-boundary
/// marker into a `PromptStructured` per ADR 0010.
pub mod prompt_loader;

/// Tool gateway: validates tool calls against agent permissions and the git-write boundary.
pub mod tool_gateway {}

/// Council deliberation state machine (Steelman gate, Devil's Advocate, synthesis).
pub mod council {}

/// Review queue manager: proposal persistence, approval/rejection, confidence tracking.
pub mod review_queue {}

/// Trust score tracker: per-agent accept/reject rates and calibration history.
pub mod trust {}

/// Agent memory store: per-agent working context between runs.
pub mod memory;

/// Conversation engine: bounded Pair-Thinking sessions.
pub mod conversation {}
