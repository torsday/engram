//! `read_index` MCP tool — the vault's Map-of-Content (MOC) index.
//!
//! Returns Cartographer's auto-maintained navigation surface: the root
//! `index.md` (a Karpathy-style one-line-per-note index) and, on request, the
//! active MOC notes (`note_type = 'moc'`).
//!
//! A translation surface over the index: reads the `index.md` row and the MOC
//! rows from the `notes` table. No logic of its own.
//!
//! ## Input schema
//!
//! ```json
//! { "mode": "root" }   // optional; "root" (default) or "all_mocs"
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "root_index_body": "## Notes\n- [[a]] one-liner\n…",
//!   "mocs": [                       // present only when mode = "all_mocs"
//!     { "path": "mocs/topic.md", "title": "Topic MOC", "body": "…" }
//!   ]
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                   | meaning                                       |
//! |------------------------|-----------------------------------------------|
//! | `bad_input`            | Unrecognised `mode`                           |
//! | `vault_not_configured` | SQLite DB not found / not accessible          |
//! | `not_available`        | No `index.md` yet (Cartographer hasn't run)   |
//! | `internal_error`       | Unexpected SQLite failure                     |

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Canonical path of the root index note, relative to the vault root.
const INDEX_PATH: &str = "index.md";

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// What slice of the index to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexMode {
    /// Just the root `index.md` body. The default.
    #[default]
    Root,
    /// The root index plus every active MOC note.
    AllMocs,
}

/// Input for `read_index`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReadIndexInput {
    /// Which slice to return. Defaults to `root`.
    pub mode: IndexMode,
}

/// A single Map-of-Content note.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Moc {
    /// Path relative to the vault root.
    pub path: String,
    /// The MOC's title.
    pub title: String,
    /// The MOC's markdown body.
    pub body: String,
}

/// Output for `read_index`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadIndexOutput {
    /// The root `index.md` body.
    pub root_index_body: String,
    /// The active MOC notes, present only when `mode = "all_mocs"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mocs: Option<Vec<Moc>>,
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
    fn vault_not_configured(msg: impl Into<String>) -> Self {
        Self {
            code: "vault_not_configured".into(),
            message: msg.into(),
        }
    }
    fn not_available(msg: impl Into<String>) -> Self {
        Self {
            code: "not_available".into(),
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

/// Read the vault's MOC index.
pub fn handle(vault_root: &Path, input: ReadIndexInput) -> Result<ReadIndexOutput, ToolError> {
    let db_path = vault_root.join(".engram").join("engram.db");
    if !db_path.exists() {
        return Err(ToolError::vault_not_configured(format!(
            "engram.db not found at {}",
            db_path.display()
        )));
    }
    let conn = Connection::open(&db_path)
        .map_err(|e| ToolError::vault_not_configured(format!("could not open engram.db: {e}")))?;

    let root_index_body = conn
        .query_row(
            "SELECT content FROM notes WHERE path = ?1",
            [INDEX_PATH],
            |r| r.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(ToolError::internal_error(format!("SQLite: {other}"))),
        })?
        .ok_or_else(|| {
            ToolError::not_available(
                "no index.md yet — Cartographer maintains it once the vault has notes to map",
            )
        })?;

    let mocs = match input.mode {
        IndexMode::Root => None,
        IndexMode::AllMocs => Some(read_mocs(&conn)?),
    };

    Ok(ReadIndexOutput {
        root_index_body,
        mocs,
    })
}

/// Read every active MOC note (`note_type = 'moc'`), ordered by path. The root
/// `index.md` is excluded even if it carries the `moc` type — it's surfaced
/// separately as `root_index_body`.
fn read_mocs(conn: &Connection) -> Result<Vec<Moc>, ToolError> {
    let mut stmt = conn
        .prepare(
            "SELECT path, title, content FROM notes \
             WHERE note_type = 'moc' AND path != ?1 ORDER BY path",
        )
        .map_err(|e| ToolError::internal_error(format!("prepare: {e}")))?;
    let rows = stmt
        .query_map([INDEX_PATH], |r| {
            Ok(Moc {
                path: r.get(0)?,
                title: r.get(1)?,
                body: r.get(2)?,
            })
        })
        .map_err(|e| ToolError::internal_error(format!("query: {e}")))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ToolError::internal_error(format!("row: {e}")))
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

    fn setup_vault(with_index: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        let engram_dir = dir.path().join(".engram");
        std::fs::create_dir_all(&engram_dir).unwrap();
        let conn = Connection::open(engram_dir.join("engram.db")).unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        if with_index {
            conn.execute(
                "INSERT INTO notes (id, path, title, note_type, content, created_by) \
                 VALUES ('idx', 'index.md', 'Index', 'moc', '## Notes\n- [[a]] first', 'cartographer')",
                [],
            )
            .unwrap();
            for (id, path, title) in [
                ("m1", "mocs/topic-a.md", "Topic A MOC"),
                ("m2", "mocs/topic-b.md", "Topic B MOC"),
            ] {
                conn.execute(
                    "INSERT INTO notes (id, path, title, note_type, content, created_by) \
                     VALUES (?1, ?2, ?3, 'moc', 'body', 'cartographer')",
                    rusqlite::params![id, path, title],
                )
                .unwrap();
            }
        }
        dir
    }

    #[test]
    fn root_mode_returns_index_only() {
        let dir = setup_vault(true);
        let out = handle(
            dir.path(),
            ReadIndexInput {
                mode: IndexMode::Root,
            },
        )
        .expect("index");
        assert!(out.root_index_body.contains("first"));
        assert!(out.mocs.is_none(), "root mode omits mocs");
    }

    #[test]
    fn all_mocs_mode_returns_mocs_excluding_root() {
        let dir = setup_vault(true);
        let out = handle(
            dir.path(),
            ReadIndexInput {
                mode: IndexMode::AllMocs,
            },
        )
        .expect("index");
        let mocs = out.mocs.expect("mocs present in all_mocs mode");
        assert_eq!(mocs.len(), 2, "the two topic MOCs, not the root index");
        assert!(mocs.iter().all(|m| m.path != "index.md"));
        assert_eq!(mocs[0].path, "mocs/topic-a.md", "ordered by path");
    }

    #[test]
    fn missing_index_is_not_available() {
        let dir = setup_vault(false);
        let err = handle(dir.path(), ReadIndexInput::default()).unwrap_err();
        assert_eq!(err.code, "not_available");
    }

    #[test]
    fn missing_db_is_vault_not_configured() {
        let dir = TempDir::new().unwrap();
        let err = handle(dir.path(), ReadIndexInput::default()).unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }

    #[test]
    fn mode_defaults_to_root() {
        // An empty JSON object deserializes to mode = root.
        let parsed: ReadIndexInput = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.mode, IndexMode::Root);
    }
}
