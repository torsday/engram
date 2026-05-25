//! `search_notes` MCP tool — hybrid semantic search over the vault.
//!
//! Wraps [`engram_index::search::hybrid_search`] as an MCP tool. This
//! module is a pure translation surface: no retrieval logic lives here.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "query": "<string>",                   // required
//!   "k":     10,                           // optional, default 10
//!   "filter": {                            // optional
//!     "tag":       "<string>",
//!     "type":      "<note_type>",
//!     "since":     "<ISO-8601 timestamp>",
//!     "author":    "<string>"
//!   }
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "results": [
//!     {
//!       "note_id":    "<ULID>",
//!       "title":      "<string>",
//!       "path":       "<vault-relative path>",
//!       "snippet":    "<excerpt with <b>…</b> markers>",
//!       "score":      0.015,
//!       "provenance": "bm25" | "dense" | "both"
//!     }
//!   ]
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                   | meaning                                        |
//! |------------------------|------------------------------------------------|
//! | `bad_input`            | Empty query or unparseable input JSON          |
//! | `vault_not_configured` | Vault root is not a directory or DB is missing |
//! | `search_error`         | SQLite / FTS5 error during search              |

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use engram_index::search::{hybrid_search, SearchError, SearchFilter, SearchResult};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `search_notes` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchNotesInput {
    /// Natural-language or FTS5-syntax query string.
    pub query: String,
    /// Maximum number of results to return (default: 10).
    #[serde(default = "default_k")]
    pub k: usize,
    /// Optional metadata filter.
    #[serde(default)]
    pub filter: FilterInput,
}

fn default_k() -> usize {
    10
}

/// Metadata filter mirroring the issue's input schema.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FilterInput {
    /// Only notes with this tag.
    pub tag: Option<String>,
    /// Only notes of this type (maps to `note_type`).
    #[serde(rename = "type")]
    pub note_type: Option<String>,
    /// Only notes modified at or after this ISO-8601 timestamp.
    pub since: Option<String>,
    /// Only notes created by this author.
    pub author: Option<String>,
}

impl From<FilterInput> for SearchFilter {
    fn from(f: FilterInput) -> Self {
        SearchFilter {
            tag: f.tag,
            note_type: f.note_type,
            since: f.since,
            author: f.author,
        }
    }
}

/// Output for the `search_notes` tool.
#[derive(Debug, Clone, Serialize)]
pub struct SearchNotesOutput {
    pub results: Vec<SearchResult>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by [`handle`].
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
    fn search_error(msg: impl Into<String>) -> Self {
        Self {
            code: "search_error".into(),
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Execute a `search_notes` request.
///
/// Opens a read-only connection to `<vault_root>/.engram/engram.db` and
/// delegates to [`hybrid_search`]. The connection is opened per-call —
/// no pooling at this layer; the daemon layer owns long-lived connections.
pub fn handle(vault_root: &Path, input: SearchNotesInput) -> Result<SearchNotesOutput, ToolError> {
    if input.query.trim().is_empty() {
        return Err(ToolError::bad_input("query must not be empty"));
    }
    if input.k == 0 {
        return Err(ToolError::bad_input("k must be at least 1"));
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

    let filter: SearchFilter = input.filter.into();
    let results = hybrid_search(&conn, &input.query, input.k, &filter).map_err(|e| match e {
        SearchError::EmptyQuery => ToolError::bad_input("query must not be empty"),
        SearchError::Rusqlite(e) => ToolError::search_error(format!("SQLite: {e}")),
    })?;

    Ok(SearchNotesOutput { results })
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

    fn setup_vault() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let engram_dir = dir.path().join(".engram");
        std::fs::create_dir_all(&engram_dir).unwrap();
        let db_path = engram_dir.join("engram.db");
        let conn = Connection::open(&db_path).unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_by) \
             VALUES ('note-abc', 'note-abc.md', 'Test Note', 'evergreen', \
                     'The answer is forty-two.', '2024-06-01T00:00:00Z', 'human')",
            [],
        )
        .unwrap();
        (dir, db_path)
    }

    #[test]
    fn returns_expected_shape() {
        let (dir, _db) = setup_vault();
        let input = SearchNotesInput {
            query: "forty".into(),
            k: 10,
            filter: FilterInput::default(),
        };
        let out = handle(dir.path(), input).unwrap();
        assert_eq!(out.results.len(), 1);
        let r = &out.results[0];
        assert_eq!(r.note_id, "note-abc");
        assert_eq!(r.title, "Test Note");
        assert_eq!(r.path, "note-abc.md");
        assert!(!r.snippet.is_empty());
        assert!(r.score > 0.0);
    }

    #[test]
    fn empty_query_returns_bad_input() {
        let (dir, _db) = setup_vault();
        let input = SearchNotesInput {
            query: "  ".into(),
            k: 10,
            filter: FilterInput::default(),
        };
        let err = handle(dir.path(), input).unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn k_zero_returns_bad_input() {
        let (dir, _db) = setup_vault();
        let input = SearchNotesInput {
            query: "forty".into(),
            k: 0,
            filter: FilterInput::default(),
        };
        let err = handle(dir.path(), input).unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn missing_db_returns_vault_not_configured() {
        let dir = TempDir::new().unwrap();
        let input = SearchNotesInput {
            query: "test".into(),
            k: 5,
            filter: FilterInput::default(),
        };
        let err = handle(dir.path(), input).unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }

    #[test]
    fn no_match_returns_empty_results() {
        let (dir, _db) = setup_vault();
        let input = SearchNotesInput {
            query: "zzzyyyxxx".into(),
            k: 10,
            filter: FilterInput::default(),
        };
        let out = handle(dir.path(), input).unwrap();
        assert!(out.results.is_empty());
    }

    #[test]
    fn filter_input_note_type_maps_through() {
        let (dir, db_path) = setup_vault();
        // Add a fleeting note
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_by) \
             VALUES ('note-fl', 'note-fl.md', 'Fleeting', 'fleeting', 'forty-two', '2024-06-01T00:00:00Z', 'human')",
            [],
        ).unwrap();

        let input = SearchNotesInput {
            query: "forty".into(),
            k: 10,
            filter: FilterInput {
                note_type: Some("evergreen".into()),
                ..Default::default()
            },
        };
        let out = handle(dir.path(), input).unwrap();
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].note_id, "note-abc");
    }
}
