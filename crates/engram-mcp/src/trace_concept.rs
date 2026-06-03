//! `trace_concept` MCP tool — how a concept has evolved across the vault.
//!
//! Surfaces the chronological trail of notes that engage a concept, so a
//! client can see how the user's thinking shifted over time: the earliest
//! note is the `draft`, the latest the `current` state, notes in between are
//! `revision`s, and any note the vault marks `status = contested` is a
//! `reversal`.
//!
//! The tool is a *translation surface* over the existing index: it reuses
//! [`engram_index::search::hybrid_search`] (the same retrieval `search_notes`
//! uses) to find the notes that engage the concept, then reads each note's
//! timestamps and status from the `notes` table to order them and classify
//! the role. It adds no retrieval logic of its own.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "concept":      "lossy compression",   // required
//!   "since":        "2024-01-01T00:00:00Z", // optional, default: all time
//!   "max_excerpts": 20                      // optional, default: 20
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "concept":   "lossy compression",
//!   "narrative": null,                       // see note below
//!   "excerpts": [
//!     {
//!       "at":      "2024-02-01T00:00:00Z",
//!       "note_id": "01JXXXXXXXXXXXXXXXXXXXXXXX",
//!       "snippet": "…matching text…",
//!       "role":    "draft"
//!     }
//!   ]
//! }
//! ```
//!
//! `narrative` is reserved for the Synthesizer's evolution narrative for the
//! concept. v1 returns `null`: the Synthesizer (#49) produces evergreen notes,
//! not yet a persisted per-concept evolution narrative, so there is nothing to
//! surface. The field is part of the schema so the surface is stable when that
//! store lands.
//!
//! ## Error codes
//!
//! | code                   | meaning                                  |
//! |------------------------|------------------------------------------|
//! | `bad_input`            | Empty `concept`, or `max_excerpts == 0`  |
//! | `vault_not_configured` | SQLite DB not found / not accessible     |
//! | `search_error`         | Unexpected SQLite / FTS failure          |

use std::path::Path;

use engram_index::search::{hybrid_search, SearchError, SearchFilter};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for `trace_concept`.
#[derive(Debug, Clone, Deserialize)]
pub struct TraceConceptInput {
    /// The concept to trace. Matched against note title + content via FTS.
    pub concept: String,
    /// Only include notes modified at or after this ISO-8601 timestamp.
    /// `None` traces over all time.
    #[serde(default)]
    pub since: Option<String>,
    /// Maximum number of excerpts to return (default 20). The most
    /// concept-relevant notes are kept, then ordered chronologically.
    #[serde(default = "default_max_excerpts")]
    pub max_excerpts: usize,
}

fn default_max_excerpts() -> usize {
    20
}

/// Where a note sits in a concept's evolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConceptRole {
    /// The earliest note engaging the concept.
    Draft,
    /// A note between the first and last — the concept being worked out.
    Revision,
    /// A note the vault marks `status = contested` — the concept was pushed
    /// back on, whenever it falls chronologically.
    Reversal,
    /// The most recent note — the concept's present state.
    Current,
}

/// A single point in a concept's evolution.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConceptExcerpt {
    /// ISO-8601 timestamp this note last changed (its `modified_at`, falling
    /// back to `created_at`). Excerpts are ordered ascending by this field.
    pub at: String,
    /// ULID of the note.
    pub note_id: String,
    /// A snippet of the matching text, from the FTS retrieval.
    pub snippet: String,
    /// The note's role in the concept's evolution.
    pub role: ConceptRole,
}

/// Output for `trace_concept`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceConceptOutput {
    /// Echoes the traced concept.
    pub concept: String,
    /// The Synthesizer's evolution narrative, when one is persisted (v1:
    /// always `None`). Serialized as `null` rather than omitted so the shape
    /// is stable for clients.
    pub narrative: Option<String>,
    /// Chronologically-ordered excerpts, earliest first.
    pub excerpts: Vec<ConceptExcerpt>,
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

/// Per-note metadata read from the `notes` table for ordering + role.
struct NoteMeta {
    at: String,
    status: Option<String>,
}

/// Trace a concept's evolution across the vault.
pub fn handle(
    vault_root: &Path,
    input: TraceConceptInput,
) -> Result<TraceConceptOutput, ToolError> {
    if input.concept.trim().is_empty() {
        return Err(ToolError::bad_input("concept must not be empty"));
    }
    if input.max_excerpts == 0 {
        return Err(ToolError::bad_input("max_excerpts must be at least 1"));
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

    // Retrieve the notes that engage the concept (same retrieval as
    // `search_notes`). Over-fetch a little headroom so the `since` filter
    // can drop stale notes without starving the result.
    let fetch = input.max_excerpts.saturating_mul(2).max(input.max_excerpts);
    let hits = hybrid_search(&conn, &input.concept, fetch, &SearchFilter::default()).map_err(
        |e| match e {
            SearchError::EmptyQuery => ToolError::bad_input("concept must not be empty"),
            SearchError::Rusqlite(e) => ToolError::search_error(format!("SQLite: {e}")),
        },
    )?;

    // Join each hit to its timestamp + status, applying the `since` filter.
    let mut rows: Vec<(String, String, NoteMeta)> = Vec::new();
    for hit in hits {
        let meta = note_meta(&conn, &hit.note_id)
            .map_err(|e| ToolError::search_error(format!("note lookup: {e}")))?;
        let Some(meta) = meta else { continue };
        if let Some(since) = &input.since {
            // ISO-8601 UTC strings compare lexicographically in time order.
            if meta.at.as_str() < since.as_str() {
                continue;
            }
        }
        rows.push((hit.note_id, hit.snippet, meta));
    }

    // Order chronologically (earliest first) and cap to the requested count.
    rows.sort_by(|a, b| a.2.at.cmp(&b.2.at).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(input.max_excerpts);

    let last = rows.len().saturating_sub(1);
    let excerpts = rows
        .into_iter()
        .enumerate()
        .map(|(i, (note_id, snippet, meta))| {
            let role = classify_role(i, last, meta.status.as_deref());
            ConceptExcerpt {
                at: meta.at,
                note_id,
                snippet,
                role,
            }
        })
        .collect();

    Ok(TraceConceptOutput {
        concept: input.concept,
        narrative: None,
        excerpts,
    })
}

/// Read a note's effective timestamp (`modified_at`, else `created_at`) and
/// `status`. Returns `None` when the note row is absent (e.g. the FTS index is
/// ahead of a delete).
fn note_meta(conn: &Connection, note_id: &str) -> rusqlite::Result<Option<NoteMeta>> {
    conn.query_row(
        "SELECT COALESCE(modified_at, created_at, ''), status FROM notes WHERE id = ?1",
        [note_id],
        |row| {
            Ok(NoteMeta {
                at: row.get::<_, String>(0)?,
                status: row.get::<_, Option<String>>(1)?,
            })
        },
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

/// Assign a [`ConceptRole`] from chronological position and note status.
///
/// A `contested` note is a [`Reversal`](ConceptRole::Reversal) wherever it
/// falls. Otherwise the earliest is the [`Draft`](ConceptRole::Draft), the
/// latest the [`Current`](ConceptRole::Current), and the rest are
/// [`Revision`](ConceptRole::Revision)s. With a single excerpt, that one note
/// is `Current` (the concept's only, present state).
fn classify_role(index: usize, last: usize, status: Option<&str>) -> ConceptRole {
    if status == Some("contested") {
        ConceptRole::Reversal
    } else if index == last {
        ConceptRole::Current
    } else if index == 0 {
        ConceptRole::Draft
    } else {
        ConceptRole::Revision
    }
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
        // Three notes engaging "compression" across time, plus one unrelated.
        let rows = [
            (
                "n-draft",
                "draft.md",
                "Lossy compression draft",
                "fleeting",
                "draft",
                "Compression drops detail to save space.",
                "2024-01-01T00:00:00Z",
            ),
            (
                "n-rev",
                "rev.md",
                "Compression revisited",
                "literature",
                "needs-review",
                "Compression is a deliberate choice of what to drop.",
                "2024-03-01T00:00:00Z",
            ),
            (
                "n-contested",
                "contested.md",
                "Compression as loss",
                "evergreen",
                "contested",
                "Maybe compression is not lossy after all — the claim is contested.",
                "2024-04-01T00:00:00Z",
            ),
            (
                "n-current",
                "current.md",
                "Editing as compression",
                "evergreen",
                "evergreen",
                "Editing is the editor's compression of intent.",
                "2024-06-01T00:00:00Z",
            ),
            (
                "n-other",
                "other.md",
                "Woodworking joinery",
                "fleeting",
                "draft",
                "Dovetail joints resist pull-apart forces.",
                "2024-05-01T00:00:00Z",
            ),
        ];
        for (id, path, title, nt, status, content, modified) in rows {
            conn.execute(
                "INSERT INTO notes (id, path, title, note_type, status, content, modified_at, created_at, created_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'human')",
                rusqlite::params![id, path, title, nt, status, content, modified],
            )
            .unwrap();
        }
        (dir, db_path)
    }

    fn trace(vault: &Path, concept: &str) -> TraceConceptOutput {
        handle(
            vault,
            TraceConceptInput {
                concept: concept.into(),
                since: None,
                max_excerpts: 20,
            },
        )
        .expect("trace must succeed")
    }

    #[test]
    fn empty_concept_is_bad_input() {
        let (dir, _) = setup_vault();
        let err = handle(
            dir.path(),
            TraceConceptInput {
                concept: "  ".into(),
                since: None,
                max_excerpts: 20,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn zero_max_excerpts_is_bad_input() {
        let (dir, _) = setup_vault();
        let err = handle(
            dir.path(),
            TraceConceptInput {
                concept: "compression".into(),
                since: None,
                max_excerpts: 0,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn missing_db_is_vault_not_configured() {
        let dir = TempDir::new().unwrap();
        let err = handle(
            dir.path(),
            TraceConceptInput {
                concept: "compression".into(),
                since: None,
                max_excerpts: 20,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }

    #[test]
    fn excerpts_are_chronological_with_roles() {
        let (dir, _) = setup_vault();
        let out = trace(dir.path(), "compression");
        assert_eq!(out.concept, "compression");
        assert!(out.narrative.is_none(), "v1 has no persisted narrative");

        // The unrelated woodworking note must not appear.
        assert!(
            out.excerpts.iter().all(|e| e.note_id != "n-other"),
            "unrelated notes are not traced"
        );
        assert!(
            out.excerpts.len() >= 3,
            "the three compression notes are traced"
        );

        // Strictly ascending by timestamp.
        let times: Vec<&str> = out.excerpts.iter().map(|e| e.at.as_str()).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable();
        assert_eq!(times, sorted, "excerpts are chronological");

        // Earliest = draft, latest = current, the contested note = reversal.
        assert_eq!(out.excerpts.first().unwrap().role, ConceptRole::Draft);
        assert_eq!(out.excerpts.last().unwrap().role, ConceptRole::Current);
        assert!(
            out.excerpts
                .iter()
                .any(|e| e.note_id == "n-contested" && e.role == ConceptRole::Reversal),
            "a contested note is classified as a reversal"
        );
    }

    #[test]
    fn since_filter_drops_older_notes() {
        let (dir, _) = setup_vault();
        let out = handle(
            dir.path(),
            TraceConceptInput {
                concept: "compression".into(),
                since: Some("2024-03-15T00:00:00Z".into()),
                max_excerpts: 20,
            },
        )
        .expect("trace");
        assert!(
            out.excerpts
                .iter()
                .all(|e| e.at.as_str() >= "2024-03-15T00:00:00Z"),
            "no excerpt predates `since`"
        );
        assert!(
            out.excerpts.iter().all(|e| e.note_id != "n-draft"),
            "the January draft is filtered out"
        );
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ConceptRole::Reversal).unwrap(),
            "\"reversal\""
        );
    }
}
