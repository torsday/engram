//! `list_tags` MCP tool — enumerate all vault tags with usage counts.
//!
//! Returns every distinct tag in the `tags` table together with its usage
//! count, the earliest note creation date it appears on (`first_used`), and
//! the latest (`last_used`). Results can be filtered by prefix and a minimum
//! count threshold.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "prefix":    "evergreen",   // optional — only tags starting with this prefix
//!   "min_count": 1              // optional, default 1
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "tags": [
//!     {
//!       "tag":        "evergreen",
//!       "count":      42,
//!       "first_used": "2024-01-01T00:00:00Z",
//!       "last_used":  "2025-06-01T00:00:00Z"
//!     }
//!   ]
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                   | meaning                               |
//! |------------------------|---------------------------------------|
//! | `bad_input`            | Negative min_count or other bad input |
//! | `vault_not_configured` | SQLite DB not found / not accessible  |
//! | `internal_error`       | Unexpected SQLite failure             |

use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `list_tags` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct ListTagsInput {
    /// Only return tags that start with this prefix (case-insensitive).
    pub prefix: Option<String>,
    /// Minimum usage count (default 1 — excludes tags with 0 uses, which
    /// shouldn't exist but is a defensive guard).
    #[serde(default = "default_min_count")]
    pub min_count: i64,
}

fn default_min_count() -> i64 {
    1
}

/// One tag entry in the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagEntry {
    /// The tag string (without leading `#`).
    pub tag: String,
    /// Number of notes carrying this tag.
    pub count: i64,
    /// ISO-8601 creation date of the earliest note with this tag, if known.
    pub first_used: Option<String>,
    /// ISO-8601 creation/modification date of the most recent note with this tag,
    /// if known.
    pub last_used: Option<String>,
}

/// Output from the `list_tags` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListTagsOutput {
    pub tags: Vec<TagEntry>,
}

/// Tool-level error (same shape as the shared `ToolError`; converted in the
/// server adapter).
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

/// Execute the `list_tags` tool.
///
/// `vault_root` must point to the directory that contains `.engram/engram.db`.
pub fn handle(vault_root: &Path, input: ListTagsInput) -> Result<ListTagsOutput, ToolError> {
    let db_path = vault_root.join(".engram").join("engram.db");
    let conn = Connection::open(&db_path).map_err(|e| {
        ToolError::vault_not_configured(format!("cannot open {}: {}", db_path.display(), e))
    })?;

    list_tags_impl(&conn, input)
}

/// Execute the `list_tags` query against an already-open connection.
/// Useful for in-process calls and testing (no filesystem lookup).
pub fn list_tags_from_conn(
    conn: &Connection,
    input: ListTagsInput,
) -> Result<ListTagsOutput, ToolError> {
    list_tags_impl(conn, input)
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

fn list_tags_impl(conn: &Connection, input: ListTagsInput) -> Result<ListTagsOutput, ToolError> {
    if input.min_count < 0 {
        return Err(ToolError::bad_input("min_count must be >= 0"));
    }
    // Build a query that counts notes per tag and gathers first/last used
    // dates from notes.created_at / notes.modified_at.
    // LOWER(tag) used for case-insensitive prefix matching on the Rust side.
    let sql = "\
        SELECT t.tag,
               COUNT(t.note_id)                     AS count,
               MIN(n.created_at)                    AS first_used,
               MAX(COALESCE(n.modified_at, n.created_at)) AS last_used
        FROM tags t
        LEFT JOIN notes n ON n.id = t.note_id
        GROUP BY t.tag
        HAVING COUNT(t.note_id) >= ?1
        ORDER BY COUNT(t.note_id) DESC, t.tag ASC";

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| ToolError::internal(e.to_string()))?;

    let rows = stmt
        .query_map(params![input.min_count], |row| {
            Ok(TagEntry {
                tag: row.get(0)?,
                count: row.get(1)?,
                first_used: row.get(2)?,
                last_used: row.get(3)?,
            })
        })
        .map_err(|e| ToolError::internal(e.to_string()))?;

    let mut tags: Vec<TagEntry> = Vec::new();
    for row in rows {
        tags.push(row.map_err(|e| ToolError::internal(e.to_string()))?);
    }

    // Apply prefix filter in Rust (case-insensitive).
    if let Some(ref prefix) = input.prefix {
        let lc = prefix.to_lowercase();
        tags.retain(|t| t.tag.to_lowercase().starts_with(&lc));
    }

    Ok(ListTagsOutput { tags })
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

    fn insert_note(conn: &Connection, id: &str, created_at: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO notes (id, path, title, note_type, created_at) \
             VALUES (?1, ?2, ?3, 'evergreen', ?4)",
            params![id, format!("{id}.md"), format!("Note {id}"), created_at],
        )
        .unwrap();
    }

    fn add_tag(conn: &Connection, note_id: &str, tag: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO tags (note_id, tag) VALUES (?1, ?2)",
            params![note_id, tag],
        )
        .unwrap();
    }

    // ── basic count ───────────────────────────────────────────────────────────

    #[test]
    fn returns_all_tags_with_counts() {
        let conn = open_migrated();
        insert_note(&conn, "n1", "2024-01-01T00:00:00Z");
        insert_note(&conn, "n2", "2024-06-01T00:00:00Z");
        add_tag(&conn, "n1", "rust");
        add_tag(&conn, "n2", "rust");
        add_tag(&conn, "n1", "design");

        let out = list_tags_from_conn(
            &conn,
            ListTagsInput {
                prefix: None,
                min_count: 1,
            },
        )
        .unwrap();

        assert_eq!(out.tags.len(), 2);
        // "rust" has higher count — sorted by count desc.
        assert_eq!(out.tags[0].tag, "rust");
        assert_eq!(out.tags[0].count, 2);
        assert_eq!(out.tags[1].tag, "design");
        assert_eq!(out.tags[1].count, 1);
    }

    // ── prefix filter ─────────────────────────────────────────────────────────

    #[test]
    fn prefix_filter_is_case_insensitive() {
        let conn = open_migrated();
        insert_note(&conn, "n1", "2024-01-01T00:00:00Z");
        insert_note(&conn, "n2", "2024-01-02T00:00:00Z");
        add_tag(&conn, "n1", "Rust");
        add_tag(&conn, "n2", "react");
        add_tag(&conn, "n1", "design");

        let out = list_tags_from_conn(
            &conn,
            ListTagsInput {
                prefix: Some("R".to_string()),
                min_count: 1,
            },
        )
        .unwrap();

        // "Rust" and "react" both start with r/R — "design" excluded.
        assert_eq!(out.tags.len(), 2);
        let tag_names: Vec<&str> = out.tags.iter().map(|t| t.tag.as_str()).collect();
        assert!(tag_names.contains(&"Rust"));
        assert!(tag_names.contains(&"react"));
    }

    // ── min_count filter ──────────────────────────────────────────────────────

    #[test]
    fn min_count_filters_low_count_tags() {
        let conn = open_migrated();
        insert_note(&conn, "n1", "2024-01-01T00:00:00Z");
        insert_note(&conn, "n2", "2024-01-02T00:00:00Z");
        add_tag(&conn, "n1", "common");
        add_tag(&conn, "n2", "common");
        add_tag(&conn, "n1", "rare");

        let out = list_tags_from_conn(
            &conn,
            ListTagsInput {
                prefix: None,
                min_count: 2,
            },
        )
        .unwrap();

        assert_eq!(out.tags.len(), 1);
        assert_eq!(out.tags[0].tag, "common");
    }

    // ── empty vault ───────────────────────────────────────────────────────────

    #[test]
    fn empty_vault_returns_empty_list() {
        let conn = open_migrated();
        let out = list_tags_from_conn(
            &conn,
            ListTagsInput {
                prefix: None,
                min_count: 1,
            },
        )
        .unwrap();
        assert!(out.tags.is_empty());
    }

    // ── first/last used dates ─────────────────────────────────────────────────

    #[test]
    fn first_and_last_used_reflect_note_dates() {
        let conn = open_migrated();
        insert_note(&conn, "early", "2023-01-01T00:00:00Z");
        insert_note(&conn, "late", "2025-01-01T00:00:00Z");
        add_tag(&conn, "early", "knowledge");
        add_tag(&conn, "late", "knowledge");

        let out = list_tags_from_conn(
            &conn,
            ListTagsInput {
                prefix: None,
                min_count: 1,
            },
        )
        .unwrap();

        assert_eq!(out.tags.len(), 1);
        let t = &out.tags[0];
        assert_eq!(t.count, 2);
        assert_eq!(t.first_used.as_deref(), Some("2023-01-01T00:00:00Z"));
        assert_eq!(t.last_used.as_deref(), Some("2025-01-01T00:00:00Z"));
    }

    // ── bad input ────────────────────────────────────────────────────────────

    #[test]
    fn negative_min_count_returns_bad_input() {
        let conn = open_migrated();
        let err = list_tags_from_conn(
            &conn,
            ListTagsInput {
                prefix: None,
                min_count: -1,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }
}
