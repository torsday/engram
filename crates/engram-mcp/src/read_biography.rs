//! `read_biography` MCP tool — the Biographer's current user model.
//!
//! Returns the contents of the Biographer's single note, `meta/biography.md`,
//! so a client can ground assistance in the user's known roles, domains, and
//! ongoing work. Read-only — the Biographer is the only writer.
//!
//! A translation surface over the index: reads the indexed `meta/biography.md`
//! row from the `notes` table (content, `modified_at`, frontmatter). It adds no
//! logic of its own beyond splitting out the section headings and lifting
//! `confidence` from the note's frontmatter.
//!
//! ## Input schema
//!
//! ```json
//! {}
//! ```
//!
//! No inputs — there is one canonical biography.
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "body":         "## Identity\n…",            // full markdown
//!   "last_updated": "2024-06-01T00:00:00Z",      // the note's modified_at
//!   "sections":     ["Identity", "Domains of expertise", "…"],
//!   "confidence":   0.86                          // from frontmatter, else 0.0
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                   | meaning                                       |
//! |------------------------|-----------------------------------------------|
//! | `vault_not_configured` | SQLite DB not found / not accessible          |
//! | `not_available`        | No `meta/biography.md` yet (Biographer hasn't |
//! |                        | written one — e.g. the vault is too sparse)   |
//! | `internal_error`       | Unexpected SQLite failure                     |

use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// The canonical path of the Biographer's note, relative to the vault root.
const BIOGRAPHY_PATH: &str = "meta/biography.md";

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for `read_biography` — none; the biography is canonical.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReadBiographyInput {}

/// Output for `read_biography`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadBiographyOutput {
    /// The full markdown body of `meta/biography.md`.
    pub body: String,
    /// ISO-8601 timestamp the biography was last written (its `modified_at`).
    pub last_updated: String,
    /// The `##` section headings, in document order (e.g. "Identity",
    /// "Domains of expertise", "Recurring themes", …).
    pub sections: Vec<String>,
    /// The Biographer's self-assessed confidence, lifted from the note's
    /// frontmatter. `0.0` when the frontmatter carries no `confidence`.
    pub confidence: f32,
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

/// Read the Biographer's current user model from the index.
pub fn handle(
    vault_root: &Path,
    _input: ReadBiographyInput,
) -> Result<ReadBiographyOutput, ToolError> {
    let db_path = vault_root.join(".engram").join("engram.db");
    if !db_path.exists() {
        return Err(ToolError::vault_not_configured(format!(
            "engram.db not found at {}",
            db_path.display()
        )));
    }
    let conn = Connection::open(&db_path)
        .map_err(|e| ToolError::vault_not_configured(format!("could not open engram.db: {e}")))?;

    let row = conn
        .query_row(
            "SELECT content, COALESCE(modified_at, created_at, ''), frontmatter \
             FROM notes WHERE path = ?1",
            [BIOGRAPHY_PATH],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(ToolError::internal_error(format!("SQLite: {other}"))),
        })?;

    let Some((body, last_updated, frontmatter)) = row else {
        return Err(ToolError::not_available(
            "no biography yet — the Biographer writes meta/biography.md once the vault has enough material",
        ));
    };

    let sections = section_headings(&body);
    let confidence = frontmatter
        .as_deref()
        .and_then(confidence_from_frontmatter)
        .unwrap_or(0.0);

    Ok(ReadBiographyOutput {
        body,
        last_updated,
        sections,
        confidence,
    })
}

/// Extract the `##`-level section headings from the markdown body, in order.
fn section_headings(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| line.strip_prefix("## ").map(|h| h.trim().to_string()))
        .filter(|h| !h.is_empty())
        .collect()
}

/// Lift a `confidence` number from the note's frontmatter, which the index
/// stores as a JSON object. Returns `None` when absent or non-numeric.
fn confidence_from_frontmatter(frontmatter: &str) -> Option<f32> {
    let value: serde_json::Value = serde_json::from_str(frontmatter).ok()?;
    value.get("confidence")?.as_f64().map(|c| c as f32)
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

    const BIO_BODY: &str = "## Identity\nA systems thinker.\n\n## Domains of expertise\nKnowledge tools.\n\n## Recurring themes\nCompression.\n";

    fn setup_vault(with_bio: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        let engram_dir = dir.path().join(".engram");
        std::fs::create_dir_all(&engram_dir).unwrap();
        let conn = Connection::open(engram_dir.join("engram.db")).unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        if with_bio {
            conn.execute(
                "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_at, created_by, frontmatter) \
                 VALUES ('bio-1', 'meta/biography.md', 'Biography', 'moc', ?1, '2024-06-01T00:00:00Z', '2024-01-01T00:00:00Z', 'biographer', ?2)",
                rusqlite::params![BIO_BODY, r#"{"confidence": 0.86}"#],
            )
            .unwrap();
        }
        dir
    }

    fn read(dir: &Path) -> Result<ReadBiographyOutput, ToolError> {
        handle(dir, ReadBiographyInput {})
    }

    #[test]
    fn returns_body_sections_and_confidence() {
        let dir = setup_vault(true);
        let out = read(dir.path()).expect("biography present");
        assert_eq!(out.body, BIO_BODY);
        assert_eq!(out.last_updated, "2024-06-01T00:00:00Z");
        assert_eq!(
            out.sections,
            vec![
                "Identity".to_string(),
                "Domains of expertise".to_string(),
                "Recurring themes".to_string()
            ]
        );
        assert!((out.confidence - 0.86).abs() < 1e-6);
    }

    #[test]
    fn missing_biography_is_not_available() {
        let dir = setup_vault(false);
        let err = read(dir.path()).unwrap_err();
        assert_eq!(err.code, "not_available");
    }

    #[test]
    fn missing_db_is_vault_not_configured() {
        let dir = TempDir::new().unwrap();
        let err = read(dir.path()).unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }

    #[test]
    fn confidence_defaults_to_zero_without_frontmatter() {
        let dir = TempDir::new().unwrap();
        let engram_dir = dir.path().join(".engram");
        std::fs::create_dir_all(&engram_dir).unwrap();
        let conn = Connection::open(engram_dir.join("engram.db")).unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_by) \
             VALUES ('bio-2', 'meta/biography.md', 'Biography', 'moc', '## Identity\nX.', '2024-06-01T00:00:00Z', 'biographer')",
            [],
        )
        .unwrap();
        let out = read(dir.path()).expect("biography present");
        assert_eq!(out.confidence, 0.0);
        assert_eq!(out.sections, vec!["Identity".to_string()]);
    }
}
