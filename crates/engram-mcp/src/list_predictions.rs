//! `list_predictions` MCP tool — the Predictor's ledger.
//!
//! Returns predictions the Predictor has logged: the claim, when it was made
//! and is due, its open/resolved status, and the confidence at claim time —
//! plus a small calibration rollup over the whole ledger.
//!
//! A translation surface over the index: reads the `predictions` table. No
//! logic of its own beyond the status/topic filters and the count rollup.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "status": "open",     // optional: "open" (default) | "resolved" | "due" | "all"
//!   "topic":  "ml",       // optional: only this topic
//!   "limit":  50          // optional, default 50
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "predictions": [
//!     {
//!       "id": "01J…", "claim": "…", "made_at": "…", "due_at": "…",
//!       "status": "pending", "resolution": null, "confidence_at_claim": 0.7
//!     }
//!   ],
//!   "calibration_summary": { "total": 12, "resolved": 4, "open": 8 }
//! }
//! ```
//!
//! `calibration_summary` is a count rollup over the *entire* ledger (not the
//! filtered page). Full per-topic Brier calibration is a Predictor follow-on —
//! it needs a resolved-correctness signal the index doesn't yet carry.
//!
//! ## Error codes
//!
//! | code                   | meaning                                   |
//! |------------------------|-------------------------------------------|
//! | `bad_input`            | Unrecognised `status`, or `limit == 0`    |
//! | `vault_not_configured` | SQLite DB not found / not accessible      |
//! | `internal_error`       | Unexpected SQLite failure                 |

use std::path::Path;

use chrono::Utc;
use rusqlite::{Connection, ToSql};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Which predictions to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusFilter {
    /// Not yet resolved. The default.
    #[default]
    Open,
    /// Resolved (has a `resolved_at`).
    Resolved,
    /// Open and past its `due_at`.
    Due,
    /// No status filter.
    All,
}

/// Input for `list_predictions`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ListPredictionsInput {
    /// Status filter (default `open`).
    pub status: StatusFilter,
    /// Restrict to a single topic.
    pub topic: Option<String>,
    /// Max predictions to return (default 50).
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

impl Default for ListPredictionsInput {
    /// `Default` must match the serde default (`limit = 50`, not `0`) — the
    /// server uses it for a null/absent `arguments` payload, and `limit == 0`
    /// is rejected as bad input.
    fn default() -> Self {
        Self {
            status: StatusFilter::Open,
            topic: None,
            limit: default_limit(),
        }
    }
}

/// A single prediction in the ledger.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prediction {
    pub id: String,
    /// The predicted claim (the note excerpt that logged it).
    pub claim: String,
    /// ISO-8601 timestamp the prediction was made.
    pub made_at: String,
    /// ISO-8601 due date, when set.
    pub due_at: Option<String>,
    /// `pending`, `resolved`, … (the ledger's own status string).
    pub status: String,
    /// The resolution note, when resolved.
    pub resolution: Option<String>,
    /// The confidence stated when the prediction was made.
    pub confidence_at_claim: Option<f64>,
}

/// A count rollup over the whole prediction ledger.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibrationSummary {
    /// Total predictions logged.
    pub total: u64,
    /// Predictions with a `resolved_at`.
    pub resolved: u64,
    /// Predictions still open.
    pub open: u64,
}

/// Output for `list_predictions`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListPredictionsOutput {
    pub predictions: Vec<Prediction>,
    /// Ledger-wide count rollup; `None` when the ledger is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration_summary: Option<CalibrationSummary>,
}

// ---------------------------------------------------------------------------
// Errors (structurally identical to the other tools; adapted in server.rs)
// ---------------------------------------------------------------------------

/// Tool-local error. `server.rs` adapts it into the shared `ToolError`.
#[derive(Debug)]
pub struct ToolError {
    pub code: String,
    pub message: String,
}

impl ToolError {
    fn bad_input(msg: impl Into<String>) -> Self {
        Self {
            code: "bad_input".into(),
            message: msg.into(),
        }
    }
    fn vault_not_configured(msg: impl Into<String>) -> Self {
        Self {
            code: "vault_not_configured".into(),
            message: msg.into(),
        }
    }
    fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: "internal_error".into(),
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// List predictions from the ledger.
pub fn handle(
    vault_root: &Path,
    input: ListPredictionsInput,
) -> Result<ListPredictionsOutput, ToolError> {
    if input.limit == 0 {
        return Err(ToolError::bad_input("limit must be at least 1"));
    }

    let db_path = vault_root.join(".engram").join("engram.db");
    if !db_path.exists() {
        return Err(ToolError::vault_not_configured(format!(
            "engram.db not found at {}",
            db_path.display()
        )));
    }
    let conn = Connection::open(&db_path)
        .map_err(|e| ToolError::vault_not_configured(format!("could not open engram.db: {e}")))?;

    // Build the WHERE clause from the status + topic filters.
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    match input.status {
        StatusFilter::Open => clauses.push("resolved_at IS NULL".into()),
        StatusFilter::Resolved => clauses.push("resolved_at IS NOT NULL".into()),
        StatusFilter::Due => {
            clauses.push("resolved_at IS NULL AND due_at IS NOT NULL AND due_at <= ?".into());
            params.push(Box::new(now));
        }
        StatusFilter::All => {}
    }
    if let Some(topic) = &input.topic {
        clauses.push("topic = ?".into());
        params.push(Box::new(topic.clone()));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };

    let sql = format!(
        "SELECT id, excerpt, claimed_at, due_at, status, resolution_note, confidence \
         FROM predictions {where_sql} ORDER BY claimed_at DESC LIMIT {}",
        input.limit
    );

    let predictions = {
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| ToolError::internal_error(format!("prepare: {e}")))?;
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |r| {
                Ok(Prediction {
                    id: r.get(0)?,
                    claim: r.get(1)?,
                    made_at: r.get(2)?,
                    due_at: r.get(3)?,
                    status: r.get(4)?,
                    resolution: r.get(5)?,
                    confidence_at_claim: r.get(6)?,
                })
            })
            .map_err(|e| ToolError::internal_error(format!("query: {e}")))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ToolError::internal_error(format!("row: {e}")))?
    };

    let calibration_summary = ledger_summary(&conn)?;

    Ok(ListPredictionsOutput {
        predictions,
        calibration_summary,
    })
}

/// Count rollup over the entire ledger. `None` when there are no predictions.
fn ledger_summary(conn: &Connection) -> Result<Option<CalibrationSummary>, ToolError> {
    let (total, resolved): (u64, u64) = conn
        .query_row(
            "SELECT COUNT(*), COUNT(resolved_at) FROM predictions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| ToolError::internal_error(format!("summary: {e}")))?;
    if total == 0 {
        return Ok(None);
    }
    Ok(Some(CalibrationSummary {
        total,
        resolved,
        open: total - resolved,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engram_index::sqlite::Migrator;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn setup_vault(seed: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        let engram_dir = dir.path().join(".engram");
        std::fs::create_dir_all(&engram_dir).unwrap();
        let conn = Connection::open(engram_dir.join("engram.db")).unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        if seed {
            // A note for the FK, then three predictions: one open+overdue, one
            // open+future, one resolved.
            conn.execute(
                "INSERT INTO notes (id, path, title, note_type, content, created_by) \
                 VALUES ('n1', 'n1.md', 'N', 'fleeting', 'x', 'human')",
                [],
            )
            .unwrap();
            let preds = [
                (
                    "p-overdue",
                    "Rates will rise",
                    "2024-01-01T00:00:00Z",
                    Some("2024-02-01T00:00:00Z"),
                    "pending",
                    None::<&str>,
                    0.7,
                ),
                (
                    "p-future",
                    "Model ships Q4",
                    "2024-03-01T00:00:00Z",
                    Some("2999-01-01T00:00:00Z"),
                    "pending",
                    None,
                    0.6,
                ),
                (
                    "p-resolved",
                    "It rained",
                    "2024-01-10T00:00:00Z",
                    Some("2024-01-20T00:00:00Z"),
                    "resolved",
                    Some("Confirmed"),
                    0.9,
                ),
            ];
            for (id, excerpt, claimed, due, status, res, conf) in preds {
                let resolved_at = if status == "resolved" {
                    Some("2024-01-21T00:00:00Z")
                } else {
                    None
                };
                conn.execute(
                    "INSERT INTO predictions (id, note_id, excerpt, claimed_at, due_at, confidence, topic, status, resolved_at, resolution_note) \
                     VALUES (?1, 'n1', ?2, ?3, ?4, ?5, 'econ', ?6, ?7, ?8)",
                    rusqlite::params![id, excerpt, claimed, due, conf, status, resolved_at, res],
                )
                .unwrap();
            }
        }
        dir
    }

    fn list(dir: &Path, status: StatusFilter) -> ListPredictionsOutput {
        handle(
            dir,
            ListPredictionsInput {
                status,
                topic: None,
                limit: 50,
            },
        )
        .expect("list")
    }

    #[test]
    fn open_excludes_resolved() {
        let out = list(setup_vault(true).path(), StatusFilter::Open);
        assert!(out.predictions.iter().all(|p| p.resolution.is_none()));
        assert!(out.predictions.iter().all(|p| p.id != "p-resolved"));
        assert_eq!(out.predictions.len(), 2);
    }

    #[test]
    fn resolved_only_resolved() {
        let out = list(setup_vault(true).path(), StatusFilter::Resolved);
        assert_eq!(out.predictions.len(), 1);
        assert_eq!(out.predictions[0].id, "p-resolved");
        assert_eq!(out.predictions[0].resolution.as_deref(), Some("Confirmed"));
    }

    #[test]
    fn due_is_open_and_overdue() {
        let out = list(setup_vault(true).path(), StatusFilter::Due);
        assert_eq!(out.predictions.len(), 1, "only the overdue-open one");
        assert_eq!(out.predictions[0].id, "p-overdue");
    }

    #[test]
    fn calibration_summary_counts_whole_ledger() {
        let out = list(setup_vault(true).path(), StatusFilter::Open);
        let s = out.calibration_summary.expect("summary present");
        assert_eq!(s.total, 3);
        assert_eq!(s.resolved, 1);
        assert_eq!(s.open, 2);
    }

    #[test]
    fn empty_ledger_has_no_summary() {
        let out = list(setup_vault(false).path(), StatusFilter::All);
        assert!(out.predictions.is_empty());
        assert!(out.calibration_summary.is_none());
    }

    #[test]
    fn topic_filter_and_limit_validation() {
        let dir = setup_vault(true);
        let none = handle(
            dir.path(),
            ListPredictionsInput {
                status: StatusFilter::All,
                topic: Some("nonexistent".into()),
                limit: 50,
            },
        )
        .unwrap();
        assert!(none.predictions.is_empty());
        let err = handle(
            dir.path(),
            ListPredictionsInput {
                status: StatusFilter::All,
                topic: None,
                limit: 0,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn missing_db_is_vault_not_configured() {
        let dir = TempDir::new().unwrap();
        let err = handle(dir.path(), ListPredictionsInput::default()).unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }
}
