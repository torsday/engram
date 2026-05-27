//! Agent action log: records every unstaged agent write to `agent_actions`
//! and reconciles human decisions when `.git/index` changes.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use engram_agents::action_log::{ActionLog, AgentAction, HumanDecision};
//! use engram_core::note_id::NoteId;
//! use rusqlite::Connection;
//! use std::sync::Arc;
//!
//! let conn = Arc::new(std::sync::Mutex::new(Connection::open_in_memory().unwrap()));
//! let log = ActionLog::new(conn);
//!
//! let action = AgentAction {
//!     id: NoteId::new(),
//!     agent_name: "linker".to_string(),
//!     kind: "link-add".to_string(),
//!     files: vec!["notes/foo.md".to_string()],
//!     diff_hash: "abc123".to_string(),
//!     confidence: 0.9,
//!     rationale: "Strong thematic overlap".to_string(),
//!     deliberation_id: None,
//!     rubric_check: "pass".to_string(),
//!     wrote_at: chrono::Utc::now(),
//!     human_decision: None,
//!     decided_at: None,
//!     final_diff_hash: None,
//!     git_commit_sha: None,
//!     parent_run_id: None,
//! };
//!
//! let action_id = log.record(action).unwrap();
//! log.resolve(&action_id, HumanDecision::Staged, None).unwrap();
//! ```
//!
//! ## Reconciliation
//!
//! When `WatchEvent::GitIndexChanged` fires, call
//! `ActionLog::reconcile_with_git(vault_root)`. It shells out `git diff
//! --name-only --cached` to find staged paths and flips matching pending rows
//! to `human_decision = 'staged'`. If the current file hash differs from the
//! stored `diff_hash` the decision is `amended` and `final_diff_hash` is
//! populated. `git restore`d files (no longer in git) stay NULL; after HEAD
//! changes the SHA is written to rows updated in that commit.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use engram_core::note_id::NoteId;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The human's verdict on an agent's proposed change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HumanDecision {
    /// The user ran `git add` on the file(s).
    Staged,
    /// The user ran `git restore` / discarded the change.
    Rejected,
    /// The user accepted but edited the content before staging.
    Amended,
    /// The user explicitly marked the action as ignored without touching the file.
    Ignored,
}

impl HumanDecision {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Rejected => "rejected",
            Self::Amended => "amended",
            Self::Ignored => "ignored",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "staged" => Some(Self::Staged),
            "rejected" => Some(Self::Rejected),
            "amended" => Some(Self::Amended),
            "ignored" => Some(Self::Ignored),
            _ => None,
        }
    }
}

/// An agent action that was (or will be) written to `agent_actions`.
///
/// Mirrors the `agent_actions` table schema from migration 001.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAction {
    /// ULID — primary key. Generate with [`NoteId::new()`].
    pub id: NoteId,
    /// Agent identifier (e.g. `"linker"`, `"tagger"`).
    pub agent_name: String,
    /// Kind of change: `link-add`, `tag-norm`, `note-create`, etc.
    pub kind: String,
    /// Relative paths touched by this action (markdown + sidecar if applicable).
    pub files: Vec<String>,
    /// SHA-256 of the unified diff.
    pub diff_hash: String,
    /// Agent's self-assessed confidence [0.0, 1.0].
    pub confidence: f64,
    /// Human-readable rationale for the action.
    pub rationale: String,
    /// ULID of the council deliberation that produced this action, if any.
    pub deliberation_id: Option<String>,
    /// Whether the rubric check passed: `"pass"`, `"fail"`, or `"n/a"`.
    pub rubric_check: String,
    /// Wall-clock time the action was written (unstaged).
    pub wrote_at: DateTime<Utc>,
    /// Human's decision (NULL until the user acts).
    pub human_decision: Option<HumanDecision>,
    /// When the human made their decision.
    pub decided_at: Option<DateTime<Utc>>,
    /// Post-amendment diff hash (set only if `human_decision == Amended`).
    pub final_diff_hash: Option<String>,
    /// Git commit SHA once the change lands in history.
    pub git_commit_sha: Option<String>,
    /// `agent_runs.id` of the parent run when this action was
    /// produced by a sub-agent invocation. `None` for top-level
    /// (human / scheduler / file-change-initiated) runs.
    ///
    /// Threaded through from [`crate::runner::AgentRunner::run_sub_agent`]
    /// so the audit trail can join a Curator's sub-Linker writes
    /// back to the Curator's parent run. See issue #31.
    #[serde(default)]
    pub parent_run_id: Option<String>,
}

/// Errors from action log operations.
#[derive(Debug, Error)]
pub enum ActionLogError {
    /// SQLite / rusqlite error.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// JSON serialisation / deserialisation error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// A git subprocess failed.
    #[error("git error: {0}")]
    Git(String),
    /// The requested action was not found.
    #[error("action not found: {0}")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// ActionLog
// ---------------------------------------------------------------------------

/// Records agent writes and reconciles human decisions with git state.
#[derive(Clone)]
pub struct ActionLog {
    conn: Arc<Mutex<Connection>>,
}

impl ActionLog {
    /// Create a new `ActionLog` using `conn` as the backing store.
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Insert an [`AgentAction`] into `agent_actions` and return its id.
    ///
    /// `human_decision`, `decided_at`, `final_diff_hash`, and `git_commit_sha`
    /// are always stored as NULL on insert — they are populated later by
    /// [`resolve`](Self::resolve) or [`reconcile_with_git`](Self::reconcile_with_git).
    pub fn record(&self, action: AgentAction) -> Result<NoteId, ActionLogError> {
        let files_json = serde_json::to_string(&action.files)?;
        let conn = self.conn.lock().map_err(|e| {
            ActionLogError::Sqlite(rusqlite::Error::InvalidParameterName(e.to_string()))
        })?;
        conn.execute(
            "INSERT INTO agent_actions \
             (id, agent_name, kind, files, diff_hash, confidence, rationale, \
              deliberation_id, rubric_check, wrote_at, parent_run_id, \
              human_decision, decided_at, final_diff_hash, git_commit_sha) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11, NULL, NULL, NULL, NULL)",
            params![
                action.id.as_str(),
                action.agent_name,
                action.kind,
                files_json,
                action.diff_hash,
                action.confidence,
                action.rationale,
                action.deliberation_id,
                action.rubric_check,
                action.wrote_at.to_rfc3339(),
                action.parent_run_id,
            ],
        )?;
        Ok(action.id)
    }

    /// Update the human decision on a specific action.
    ///
    /// Optionally stores `git_commit_sha` when the change lands in history.
    pub fn resolve(
        &self,
        action_id: &NoteId,
        decision: HumanDecision,
        commit_sha: Option<&str>,
    ) -> Result<(), ActionLogError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().map_err(|e| {
            ActionLogError::Sqlite(rusqlite::Error::InvalidParameterName(e.to_string()))
        })?;
        let rows = conn.execute(
            "UPDATE agent_actions \
             SET human_decision = ?1, decided_at = ?2, git_commit_sha = COALESCE(?3, git_commit_sha) \
             WHERE id = ?4",
            params![decision.as_str(), now, commit_sha, action_id.as_str()],
        )?;
        if rows == 0 {
            return Err(ActionLogError::NotFound(action_id.as_str().to_string()));
        }
        Ok(())
    }

    /// Return all actions with `human_decision IS NULL` (awaiting human review).
    pub fn pending(&self) -> Result<Vec<AgentAction>, ActionLogError> {
        let conn = self.conn.lock().map_err(|e| {
            ActionLogError::Sqlite(rusqlite::Error::InvalidParameterName(e.to_string()))
        })?;
        self.query_rows(
            &conn,
            "SELECT * FROM agent_actions WHERE human_decision IS NULL ORDER BY wrote_at",
            [],
        )
    }

    /// Return all actions, optionally filtered by agent name and/or start time.
    pub fn history(
        &self,
        agent: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<AgentAction>, ActionLogError> {
        let conn = self.conn.lock().map_err(|e| {
            ActionLogError::Sqlite(rusqlite::Error::InvalidParameterName(e.to_string()))
        })?;
        match (agent, since) {
            (None, None) => {
                self.query_rows(&conn, "SELECT * FROM agent_actions ORDER BY wrote_at", [])
            }
            (Some(a), None) => {
                let mut stmt = conn.prepare(
                    "SELECT * FROM agent_actions WHERE agent_name = ?1 ORDER BY wrote_at",
                )?;
                let rows = stmt.query_map(params![a], row_to_action)?;
                collect_rows(rows)
            }
            (None, Some(s)) => {
                let since_str = s.to_rfc3339();
                let mut stmt = conn.prepare(
                    "SELECT * FROM agent_actions WHERE wrote_at >= ?1 ORDER BY wrote_at",
                )?;
                let rows = stmt.query_map(params![since_str], row_to_action)?;
                collect_rows(rows)
            }
            (Some(a), Some(s)) => {
                let since_str = s.to_rfc3339();
                let mut stmt = conn.prepare(
                    "SELECT * FROM agent_actions \
                     WHERE agent_name = ?1 AND wrote_at >= ?2 ORDER BY wrote_at",
                )?;
                let rows = stmt.query_map(params![a, since_str], row_to_action)?;
                collect_rows(rows)
            }
        }
    }

    /// Reconcile pending actions against the current git index state.
    ///
    /// Called when `WatchEvent::GitIndexChanged` fires. Shells out to `git`
    /// to determine which paths are staged, then updates the corresponding
    /// pending action rows.
    ///
    /// Decision rules:
    /// - Path in `git diff --cached --name-only` → `staged`
    /// - Path staged but file hash differs from `diff_hash` → `amended`
    ///   (stores post-amend hash in `final_diff_hash`)
    ///
    /// After HEAD changes, call [`record_commit_sha`](Self::record_commit_sha)
    /// to populate `git_commit_sha` on the affected rows.
    ///
    /// Manual edits (user edits a file with no pending agent action) are
    /// intentionally not logged — this method only acts on rows with
    /// `human_decision IS NULL`.
    pub fn reconcile_with_git(&self, vault_root: &Path) -> Result<(), ActionLogError> {
        // Get staged paths from git.
        let output = std::process::Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(vault_root)
            .output()
            .map_err(|e| ActionLogError::Git(e.to_string()))?;
        if !output.status.success() {
            return Err(ActionLogError::Git(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        let staged_paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect();

        if staged_paths.is_empty() {
            return Ok(());
        }

        let now = Utc::now().to_rfc3339();

        let conn = self.conn.lock().map_err(|e| {
            ActionLogError::Sqlite(rusqlite::Error::InvalidParameterName(e.to_string()))
        })?;

        // For each pending action, check if any of its files are now staged.
        let mut stmt = conn.prepare(
            "SELECT id, files, diff_hash FROM agent_actions WHERE human_decision IS NULL",
        )?;
        let candidates: Vec<(String, Vec<String>, String)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    serde_json::from_str::<Vec<String>>(&row.get::<_, String>(1)?)
                        .unwrap_or_default(),
                    row.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        for (id, files, diff_hash) in candidates {
            let any_staged = files.iter().any(|f| staged_paths.contains(f));
            if !any_staged {
                continue;
            }

            // Check if any staged file's current hash differs from diff_hash.
            let amended = files.iter().filter(|f| staged_paths.contains(*f)).any(|f| {
                let path = vault_root.join(f);
                if let Ok(contents) = std::fs::read(&path) {
                    let hash = sha256_hex(&contents);
                    hash != diff_hash
                } else {
                    false
                }
            });

            if amended {
                // Compute the new hash from the staged version.
                let final_hash = files
                    .iter()
                    .filter(|f| staged_paths.contains(*f))
                    .find_map(|f| {
                        let path = vault_root.join(f);
                        std::fs::read(&path).ok().map(|b| sha256_hex(&b))
                    });
                conn.execute(
                    "UPDATE agent_actions \
                     SET human_decision = 'amended', decided_at = ?1, final_diff_hash = ?2 \
                     WHERE id = ?3",
                    params![now, final_hash, id],
                )?;
            } else {
                conn.execute(
                    "UPDATE agent_actions \
                     SET human_decision = 'staged', decided_at = ?1 \
                     WHERE id = ?2",
                    params![now, id],
                )?;
            }
        }

        Ok(())
    }

    /// Populate `git_commit_sha` on all actions decided in the most recent
    /// commit (i.e., rows with `human_decision IN ('staged','amended')` and
    /// `git_commit_sha IS NULL`).
    ///
    /// Call this when HEAD changes (detected via `WatchEvent::GitIndexChanged`
    /// following a commit).
    pub fn record_commit_sha(&self, vault_root: &Path) -> Result<(), ActionLogError> {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(vault_root)
            .output()
            .map_err(|e| ActionLogError::Git(e.to_string()))?;
        if !output.status.success() {
            return Ok(()); // Not yet in a git repo or no commits — skip silently.
        }
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Ok(());
        }

        let conn = self.conn.lock().map_err(|e| {
            ActionLogError::Sqlite(rusqlite::Error::InvalidParameterName(e.to_string()))
        })?;
        conn.execute(
            "UPDATE agent_actions SET git_commit_sha = ?1 \
             WHERE human_decision IN ('staged', 'amended') AND git_commit_sha IS NULL",
            params![sha],
        )?;
        Ok(())
    }

    // ── private helpers ──────────────────────────────────────────────────────

    fn query_rows(
        &self,
        conn: &Connection,
        sql: &str,
        _params: impl rusqlite::Params,
    ) -> Result<Vec<AgentAction>, ActionLogError> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], row_to_action)?;
        collect_rows(rows)
    }
}

// ---------------------------------------------------------------------------
// Row deserialisation
// ---------------------------------------------------------------------------

fn row_to_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentAction> {
    let id_str: String = row.get(0)?;
    let id = NoteId::parse(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let files_json: String = row.get(3)?;
    let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();

    let wrote_at_str: String = row.get(9)?;
    let wrote_at: DateTime<Utc> = wrote_at_str
        .parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| Utc::now());

    let human_decision_str: Option<String> = row.get(10)?;
    let human_decision = human_decision_str.and_then(|s| HumanDecision::from_str(&s));

    let decided_at_str: Option<String> = row.get(11)?;
    let decided_at = decided_at_str.and_then(|s| s.parse::<DateTime<Utc>>().ok());

    Ok(AgentAction {
        id,
        agent_name: row.get(1)?,
        kind: row.get(2)?,
        files,
        diff_hash: row.get(4)?,
        confidence: row.get(5)?,
        rationale: row.get(6)?,
        deliberation_id: row.get(7)?,
        rubric_check: row.get(8)?,
        wrote_at,
        human_decision,
        decided_at,
        final_diff_hash: row.get(12)?,
        git_commit_sha: row.get(13)?,
        // Column index 14 = `parent_run_id` (added by migration 006).
        // Returns Ok(None) if the column doesn't exist (older DBs);
        // returns Ok(Some) when migration 006 has been applied.
        parent_run_id: row.get(14).ok(),
    })
}

fn collect_rows(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<AgentAction>>,
) -> Result<Vec<AgentAction>, ActionLogError> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    format!("{hash:x}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engram_index::sqlite::Migrator;
    use rusqlite::Connection;

    fn make_log() -> ActionLog {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        ActionLog::new(Arc::new(Mutex::new(conn)))
    }

    fn make_action(kind: &str, agent: &str) -> AgentAction {
        AgentAction {
            id: NoteId::new(),
            agent_name: agent.to_string(),
            kind: kind.to_string(),
            files: vec!["notes/test.md".to_string()],
            diff_hash: "deadbeef".to_string(),
            confidence: 0.85,
            rationale: "Test rationale".to_string(),
            deliberation_id: None,
            rubric_check: "pass".to_string(),
            wrote_at: Utc::now(),
            human_decision: None,
            decided_at: None,
            final_diff_hash: None,
            git_commit_sha: None,
            parent_run_id: None,
        }
    }

    // ── record ────────────────────────────────────────────────────────────────

    #[test]
    fn record_returns_action_id() {
        let log = make_log();
        let action = make_action("link-add", "linker");
        let id = log.record(action.clone()).unwrap();
        assert_eq!(id, action.id);
    }

    // ── pending ───────────────────────────────────────────────────────────────

    #[test]
    fn pending_returns_unresolved_actions() {
        let log = make_log();
        let a1 = make_action("link-add", "linker");
        let a2 = make_action("tag-norm", "tagger");
        log.record(a1.clone()).unwrap();
        log.record(a2.clone()).unwrap();

        let pending = log.pending().unwrap();
        assert_eq!(pending.len(), 2);
        // After resolving one, only the other is pending.
        log.resolve(&a1.id, HumanDecision::Staged, None).unwrap();
        let pending = log.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, a2.id);
    }

    // ── resolve ───────────────────────────────────────────────────────────────

    #[test]
    fn resolve_sets_decision_and_decided_at() {
        let log = make_log();
        let action = make_action("link-add", "linker");
        let id = log.record(action).unwrap();
        log.resolve(&id, HumanDecision::Staged, None).unwrap();

        let history = log.history(None, None).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].human_decision, Some(HumanDecision::Staged));
        assert!(history[0].decided_at.is_some());
    }

    #[test]
    fn resolve_with_commit_sha_stores_sha() {
        let log = make_log();
        let action = make_action("link-add", "linker");
        let id = log.record(action).unwrap();
        log.resolve(&id, HumanDecision::Staged, Some("abc123"))
            .unwrap();

        let history = log.history(None, None).unwrap();
        assert_eq!(history[0].git_commit_sha, Some("abc123".to_string()));
    }

    #[test]
    fn resolve_unknown_id_returns_not_found() {
        let log = make_log();
        let fake_id = NoteId::new();
        let err = log
            .resolve(&fake_id, HumanDecision::Rejected, None)
            .unwrap_err();
        assert!(matches!(err, ActionLogError::NotFound(_)));
    }

    // ── history ───────────────────────────────────────────────────────────────

    #[test]
    fn history_filter_by_agent() {
        let log = make_log();
        log.record(make_action("link-add", "linker")).unwrap();
        log.record(make_action("tag-norm", "tagger")).unwrap();
        log.record(make_action("link-add", "linker")).unwrap();

        let linker_history = log.history(Some("linker"), None).unwrap();
        assert_eq!(linker_history.len(), 2);
        assert!(linker_history.iter().all(|a| a.agent_name == "linker"));
    }

    #[test]
    fn history_filter_by_since() {
        let log = make_log();
        let action = make_action("link-add", "linker");
        log.record(action).unwrap();

        let future = Utc::now() + chrono::Duration::hours(1);
        let empty = log.history(None, Some(future)).unwrap();
        assert!(empty.is_empty());

        let past = Utc::now() - chrono::Duration::hours(1);
        let found = log.history(None, Some(past)).unwrap();
        assert_eq!(found.len(), 1);
    }

    // ── round-trip serialisation ──────────────────────────────────────────────

    #[test]
    fn multi_file_action_round_trips() {
        let log = make_log();
        let mut action = make_action("note-create", "curator");
        action.files = vec![
            "notes/foo.md".to_string(),
            ".engram/sidecar/foo.json".to_string(),
        ];
        action.deliberation_id = Some("01DELIBERATION".to_string());

        log.record(action.clone()).unwrap();
        let history = log.history(None, None).unwrap();
        assert_eq!(history[0].files, action.files);
        assert_eq!(history[0].deliberation_id, action.deliberation_id);
    }
}
