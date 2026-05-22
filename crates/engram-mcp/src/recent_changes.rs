//! `recent_changes` MCP tool — notes modified within a time window.
//!
//! Returns vault notes changed since a given ISO-8601 timestamp, optionally
//! filtered by author kind (human, agent, or any) and ordered by recency.
//!
//! The data is sourced from two tables:
//! - `notes` — captures human-authored creates and modifies (`created_at`,
//!   `modified_at`)
//! - `agent_actions` — captures agent-proposed writes (`wrote_at`)
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "since":  "2024-01-01T00:00:00Z",  // optional, default: 24h ago
//!   "limit":  50,                       // optional, default: 50
//!   "author": "any"                     // optional: "human"|"agent"|"any"
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "changes": [
//!     {
//!       "note_id":     "01JXXXXXXXXXXXXXXXXXXXXXXX",
//!       "path":        "notes/some-note.md",
//!       "change_type": "modified",
//!       "at":          "2024-06-01T12:00:00Z",
//!       "author":      "agent",
//!       "agent_name":  "linker"
//!     }
//!   ]
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                   | meaning                               |
//! |------------------------|---------------------------------------|
//! | `bad_input`            | Unrecognised `author` value, bad date  |
//! | `vault_not_configured` | SQLite DB not found / not accessible  |
//! | `internal_error`       | Unexpected SQLite failure             |

use std::path::Path;

use chrono::{Duration, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Author filter for `recent_changes`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthorFilter {
    Human,
    Agent,
    #[default]
    Any,
}

/// Input for the `recent_changes` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct RecentChangesInput {
    /// Only return changes after this ISO-8601 timestamp. Defaults to 24 h ago.
    pub since: Option<String>,
    /// Maximum number of entries to return (default 50).
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Filter by author kind. Defaults to `"any"`.
    #[serde(default)]
    pub author: AuthorFilter,
}

fn default_limit() -> i64 {
    50
}

/// One change entry in the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeEntry {
    /// ULID of the note.
    pub note_id: String,
    /// Vault-relative path (e.g. `"notes/foo.md"`).
    pub path: String,
    /// Kind of change: `"created"`, `"modified"`, or `"agent-proposed"`.
    pub change_type: String,
    /// ISO-8601 timestamp.
    pub at: String,
    /// `"human"` or `"agent"`.
    pub author: String,
    /// Agent name (only set when `author == "agent"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}

/// Output from `recent_changes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentChangesOutput {
    pub changes: Vec<ChangeEntry>,
}

/// Tool-level error (converted to shared `ToolError` in the server adapter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: "internal_error".into(),
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Execute the `recent_changes` tool against the vault at `vault_root`.
pub fn handle(
    vault_root: &Path,
    input: RecentChangesInput,
) -> Result<RecentChangesOutput, ToolError> {
    let db_path = vault_root.join(".engram").join("engram.db");
    let conn = Connection::open(&db_path).map_err(|e| {
        ToolError::vault_not_configured(format!("cannot open {}: {}", db_path.display(), e))
    })?;
    recent_changes_impl(&conn, input)
}

/// Execute the query against an already-open connection (for tests and in-process callers).
pub fn recent_changes_from_conn(
    conn: &Connection,
    input: RecentChangesInput,
) -> Result<RecentChangesOutput, ToolError> {
    recent_changes_impl(conn, input)
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

fn recent_changes_impl(
    conn: &Connection,
    input: RecentChangesInput,
) -> Result<RecentChangesOutput, ToolError> {
    if input.limit < 1 {
        return Err(ToolError::bad_input("limit must be >= 1"));
    }

    // Resolve `since` — default to 24h ago.
    let since_str = if let Some(ref s) = input.since {
        // Validate the provided timestamp by parsing it.
        s.parse::<chrono::DateTime<Utc>>()
            .map_err(|_| ToolError::bad_input(format!("invalid ISO-8601 date: {s}")))?;
        s.clone()
    } else {
        (Utc::now() - Duration::hours(24)).to_rfc3339()
    };

    let mut changes: Vec<ChangeEntry> = Vec::new();

    // ── Human changes from `notes` ────────────────────────────────────────
    if input.author == AuthorFilter::Human || input.author == AuthorFilter::Any {
        // Notes created since `since`.
        let mut stmt = conn
            .prepare(
                "SELECT id, path, created_at FROM notes \
                 WHERE created_at IS NOT NULL AND created_at >= ?1 \
                 ORDER BY created_at DESC",
            )
            .map_err(|e| ToolError::internal(e.to_string()))?;

        let rows = stmt
            .query_map(params![since_str], |row| {
                Ok(ChangeEntry {
                    note_id: row.get(0)?,
                    path: row.get(1)?,
                    change_type: "created".to_string(),
                    at: row.get(2)?,
                    author: "human".to_string(),
                    agent_name: None,
                })
            })
            .map_err(|e| ToolError::internal(e.to_string()))?;

        for row in rows {
            changes.push(row.map_err(|e| ToolError::internal(e.to_string()))?);
        }

        // Notes modified since `since` (excluding those that were just created
        // at the same timestamp — prefer the "created" entry).
        let mut stmt = conn
            .prepare(
                "SELECT id, path, modified_at FROM notes \
                 WHERE modified_at IS NOT NULL AND modified_at >= ?1 \
                 AND (created_at IS NULL OR modified_at != created_at) \
                 ORDER BY modified_at DESC",
            )
            .map_err(|e| ToolError::internal(e.to_string()))?;

        let rows = stmt
            .query_map(params![since_str], |row| {
                Ok(ChangeEntry {
                    note_id: row.get(0)?,
                    path: row.get(1)?,
                    change_type: "modified".to_string(),
                    at: row.get(2)?,
                    author: "human".to_string(),
                    agent_name: None,
                })
            })
            .map_err(|e| ToolError::internal(e.to_string()))?;

        for row in rows {
            changes.push(row.map_err(|e| ToolError::internal(e.to_string()))?);
        }
    }

    // ── Agent changes from `agent_actions` ────────────────────────────────
    if input.author == AuthorFilter::Agent || input.author == AuthorFilter::Any {
        // Each agent action may touch multiple files. Emit one entry per file.
        // Fetch the JSON blob and expand in Rust; simpler than json_each JOIN.
        {
            let mut stmt2 = conn
                .prepare(
                    "SELECT agent_name, files, wrote_at FROM agent_actions \
                     WHERE wrote_at >= ?1 ORDER BY wrote_at DESC",
                )
                .map_err(|e| ToolError::internal(e.to_string()))?;

            struct RawAction {
                agent_name: String,
                files_json: String,
                wrote_at: String,
            }

            let raw: Vec<RawAction> = stmt2
                .query_map(params![since_str], |row| {
                    Ok(RawAction {
                        agent_name: row.get(0)?,
                        files_json: row.get(1)?,
                        wrote_at: row.get(2)?,
                    })
                })
                .map_err(|e| ToolError::internal(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            for action in raw {
                let files: Vec<String> =
                    serde_json::from_str(&action.files_json).unwrap_or_default();
                for file_path in files {
                    // Try to resolve note_id from path.
                    let note_id: Option<String> = conn
                        .query_row(
                            "SELECT id FROM notes WHERE path = ?1",
                            params![file_path],
                            |row| row.get(0),
                        )
                        .ok();
                    changes.push(ChangeEntry {
                        note_id: note_id.unwrap_or_default(),
                        path: file_path,
                        change_type: "agent-proposed".to_string(),
                        at: action.wrote_at.clone(),
                        author: "agent".to_string(),
                        agent_name: Some(action.agent_name.clone()),
                    });
                }
            }
        }
    }

    // Sort by `at` descending and apply limit.
    changes.sort_by(|a, b| b.at.cmp(&a.at));
    changes.truncate(input.limit as usize);

    Ok(RecentChangesOutput { changes })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engram_index::sqlite::Migrator;
    use rusqlite::{params, Connection};

    fn open_migrated() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        conn
    }

    fn insert_note(
        conn: &Connection,
        id: &str,
        path: &str,
        created_at: &str,
        modified_at: Option<&str>,
    ) {
        conn.execute(
            "INSERT OR IGNORE INTO notes (id, path, title, note_type, created_at, modified_at) \
             VALUES (?1, ?2, ?3, 'evergreen', ?4, ?5)",
            params![id, path, format!("Note {id}"), created_at, modified_at],
        )
        .unwrap();
    }

    fn insert_agent_action(conn: &Connection, agent: &str, files: &[&str], wrote_at: &str) {
        let files_json = serde_json::to_string(files).unwrap();
        conn.execute(
            "INSERT INTO agent_actions \
             (id, agent_name, kind, files, diff_hash, confidence, rationale, rubric_check, wrote_at) \
             VALUES (?1, ?2, 'link-add', ?3, 'abc', 0.9, 'test', 'pass', ?4)",
            params![format!("01ACTION{agent}"), agent, files_json, wrote_at],
        )
        .unwrap();
    }

    // ── returns created notes ─────────────────────────────────────────────────

    #[test]
    fn returns_human_created_notes_in_window() {
        let conn = open_migrated();
        let since = "2024-01-01T00:00:00Z";
        insert_note(&conn, "n1", "notes/a.md", "2024-06-01T00:00:00Z", None);
        insert_note(&conn, "n2", "notes/b.md", "2023-01-01T00:00:00Z", None); // before window

        let out = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some(since.to_string()),
                limit: 50,
                author: AuthorFilter::Human,
            },
        )
        .unwrap();

        assert_eq!(out.changes.len(), 1);
        assert_eq!(out.changes[0].note_id, "n1");
        assert_eq!(out.changes[0].change_type, "created");
        assert_eq!(out.changes[0].author, "human");
    }

    // ── returns modified notes ────────────────────────────────────────────────

    #[test]
    fn returns_human_modified_notes() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "m1",
            "notes/m.md",
            "2023-01-01T00:00:00Z",
            Some("2024-06-01T12:00:00Z"),
        );

        let out = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some("2024-01-01T00:00:00Z".to_string()),
                limit: 50,
                author: AuthorFilter::Human,
            },
        )
        .unwrap();

        // Should see a "modified" entry (created_at is before window).
        assert!(out
            .changes
            .iter()
            .any(|c| c.change_type == "modified" && c.note_id == "m1"));
    }

    // ── agent filter ──────────────────────────────────────────────────────────

    #[test]
    fn agent_filter_returns_agent_actions_only() {
        let conn = open_migrated();
        insert_note(&conn, "n1", "notes/a.md", "2024-06-01T00:00:00Z", None);
        insert_agent_action(&conn, "linker", &["notes/x.md"], "2024-06-01T06:00:00Z");

        let out = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some("2024-01-01T00:00:00Z".to_string()),
                limit: 50,
                author: AuthorFilter::Agent,
            },
        )
        .unwrap();

        assert!(out.changes.iter().all(|c| c.author == "agent"));
        assert!(out
            .changes
            .iter()
            .any(|c| c.agent_name.as_deref() == Some("linker")));
    }

    // ── limit ─────────────────────────────────────────────────────────────────

    #[test]
    fn limit_caps_results() {
        let conn = open_migrated();
        for i in 0..5 {
            insert_note(
                &conn,
                &format!("n{i}"),
                &format!("notes/{i}.md"),
                &format!("2024-0{}-01T00:00:00Z", i + 1),
                None,
            );
        }
        let out = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some("2020-01-01T00:00:00Z".to_string()),
                limit: 3,
                author: AuthorFilter::Human,
            },
        )
        .unwrap();
        assert!(out.changes.len() <= 3);
    }

    // ── empty window ──────────────────────────────────────────────────────────

    #[test]
    fn empty_window_returns_empty_list() {
        let conn = open_migrated();
        let out = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some("2099-01-01T00:00:00Z".to_string()),
                limit: 50,
                author: AuthorFilter::Any,
            },
        )
        .unwrap();
        assert!(out.changes.is_empty());
    }

    // ── bad inputs ────────────────────────────────────────────────────────────

    #[test]
    fn bad_since_returns_bad_input_error() {
        let conn = open_migrated();
        let err = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some("not-a-date".to_string()),
                limit: 50,
                author: AuthorFilter::Any,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn zero_limit_returns_bad_input() {
        let conn = open_migrated();
        let err = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: None,
                limit: 0,
                author: AuthorFilter::Any,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    // ── sort order ────────────────────────────────────────────────────────────

    #[test]
    fn results_sorted_newest_first() {
        let conn = open_migrated();
        insert_note(&conn, "old", "notes/old.md", "2024-01-01T00:00:00Z", None);
        insert_note(&conn, "new", "notes/new.md", "2024-06-01T00:00:00Z", None);

        let out = recent_changes_from_conn(
            &conn,
            RecentChangesInput {
                since: Some("2020-01-01T00:00:00Z".to_string()),
                limit: 50,
                author: AuthorFilter::Human,
            },
        )
        .unwrap();

        assert!(!out.changes.is_empty());
        // Newest should appear first.
        let ats: Vec<&str> = out.changes.iter().map(|c| c.at.as_str()).collect();
        let mut sorted = ats.clone();
        sorted.sort_by(|a, b| b.cmp(a)); // descending
        assert_eq!(ats, sorted, "changes must be sorted newest-first");
    }
}
