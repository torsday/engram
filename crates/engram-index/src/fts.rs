//! SQLite FTS5 full-text search over vault notes (BM25 scoring).
//!
//! The `notes_fts` virtual table is created and kept in sync by the SQL
//! migration in `migrations/002_indexes_and_views.sql`. This module provides
//! the Rust API on top of it.
//!
//! ## Query syntax
//!
//! FTS5's default `unicode61` tokenizer handles:
//!
//! - **Term search:** `atomic habits` — both terms must appear.
//! - **Phrase search:** `"atomic habits"` — exact phrase.
//! - **Prefix search:** `atom*` — matches any token starting with `atom`.
//! - **Column-scoped search:** `title:habits` — restrict to `title` column.
//!
//! Synonyms and stemming are not included in v1; the cross-encoder reranking
//! stage in the hybrid retrieval pipeline handles semantic widening.
//!
//! ## Example
//!
//! ```rust,no_run
//! use engram_index::fts::{search_fts, FtsHit};
//! use rusqlite::Connection;
//!
//! let conn = Connection::open_in_memory().unwrap();
//! // … apply migrations …
//! let hits = search_fts(&conn, "atomic habits", 10).unwrap();
//! for hit in &hits {
//!     println!("{}: {:.3} — {}", hit.note_id, hit.bm25_score, hit.snippet);
//! }
//! ```

use rusqlite::{params, Connection};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from FTS5 search operations.
#[derive(Debug, Error)]
pub enum FtsError {
    /// SQLite / rusqlite error.
    #[error("SQLite error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    /// The query string was empty or contained only whitespace.
    #[error("FTS query must not be empty")]
    EmptyQuery,
}

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// One FTS5 search result.
#[derive(Debug, Clone, PartialEq)]
pub struct FtsHit {
    /// ULID from the `notes.id` column.
    pub note_id: String,
    /// BM25 score. FTS5 returns negative values (lower = better); we negate
    /// them so callers see positive scores where higher is better.
    pub bm25_score: f64,
    /// Short excerpt around the best matching region, with HTML-like markers
    /// (`<b>…</b>`) around the matching tokens.
    pub snippet: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Execute a BM25 full-text search over `notes_fts`.
///
/// Returns up to `limit` results ordered by relevance (best first).
/// The query uses FTS5's native syntax; see module-level docs.
///
/// # Errors
///
/// Returns [`FtsError::EmptyQuery`] if `query` is blank, or
/// [`FtsError::Rusqlite`] on any SQLite error.
pub fn search_fts(conn: &Connection, query: &str, limit: usize) -> Result<Vec<FtsHit>, FtsError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(FtsError::EmptyQuery);
    }

    // BM25 returns negative values; ORDER BY ascending puts the best match
    // first. We negate in the SELECT so callers see positive scores.
    let sql = "\
        SELECT n.id, -bm25(notes_fts), \
               snippet(notes_fts, 1, '<b>', '</b>', '…', 10) \
        FROM notes_fts \
        JOIN notes n ON notes_fts.rowid = n.rowid \
        WHERE notes_fts MATCH ?1 \
        ORDER BY bm25(notes_fts) \
        LIMIT ?2";

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![trimmed, limit as i64], |row| {
        Ok(FtsHit {
            note_id: row.get(0)?,
            bm25_score: row.get(1)?,
            snippet: row.get(2)?,
        })
    })?;

    let mut hits = Vec::new();
    for row in rows {
        hits.push(row?);
    }
    Ok(hits)
}

/// Insert or update a note in `notes_fts`.
///
/// This is called after a manual insert that bypassed the triggers (e.g.
/// during index rebuild from the vault). Normal inserts via `notes` are
/// handled automatically by the `notes_ai` trigger.
pub fn index_note(
    conn: &Connection,
    rowid: i64,
    title: &str,
    content: &str,
) -> Result<(), FtsError> {
    conn.execute(
        "INSERT INTO notes_fts (rowid, title, content) VALUES (?1, ?2, ?3)",
        params![rowid, title, content],
    )?;
    Ok(())
}

/// Remove a note from `notes_fts` by rowid.
///
/// Used during index rebuild. Normal deletes via `notes` are handled by
/// the `notes_ad` trigger.
pub fn deindex_note(
    conn: &Connection,
    rowid: i64,
    title: &str,
    content: &str,
) -> Result<(), FtsError> {
    conn.execute(
        "INSERT INTO notes_fts (notes_fts, rowid, title, content) VALUES ('delete', ?1, ?2, ?3)",
        params![rowid, title, content],
    )?;
    Ok(())
}

/// Rebuild the `notes_fts` index from scratch.
///
/// Drops all FTS rows and repopulates from `notes`. Useful after bulk imports
/// or if the index becomes inconsistent.
pub fn rebuild_index(conn: &Connection) -> Result<(), FtsError> {
    conn.execute_batch("INSERT INTO notes_fts (notes_fts) VALUES ('rebuild');")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::Migrator;
    use rusqlite::Connection;

    fn open_migrated() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        conn
    }

    fn insert_note(conn: &Connection, id: &str, title: &str, content: &str) {
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content) VALUES (?1, ?2, ?3, 'evergreen', ?4)",
            params![id, format!("{id}.md"), title, content],
        )
        .unwrap();
    }

    // ── exact-term hit ────────────────────────────────────────────────────────

    #[test]
    fn exact_term_returns_matching_note() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "note-1",
            "Atomic Habits",
            "Small changes compound over time.",
        );
        insert_note(
            &conn,
            "note-2",
            "Deep Work",
            "Distraction is the enemy of focus.",
        );

        let hits = search_fts(&conn, "compound", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, "note-1");
        assert!(hits[0].bm25_score > 0.0);
    }

    // ── phrase hit ────────────────────────────────────────────────────────────

    #[test]
    fn phrase_search_finds_exact_phrase() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "p1",
            "Habit Formation",
            "The habit loop drives all behavior.",
        );
        insert_note(
            &conn,
            "p2",
            "Habit Loop",
            "Cue routine reward is the classic model.",
        );

        let hits = search_fts(&conn, "\"habit loop\"", 10).unwrap();
        // Both notes contain "habit" but only p2's title + content contain "habit loop" as a phrase.
        assert!(!hits.is_empty());
        assert!(hits.iter().any(|h| h.note_id == "p2"));
    }

    // ── prefix hit ────────────────────────────────────────────────────────────

    #[test]
    fn prefix_search_matches_prefix() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "pre-1",
            "Atomic Note",
            "Atomicity is a design principle.",
        );
        insert_note(&conn, "pre-2", "Unrelated", "Nothing relevant here.");

        let hits = search_fts(&conn, "atom*", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, "pre-1");
    }

    // ── no-hit ────────────────────────────────────────────────────────────────

    #[test]
    fn no_matching_notes_returns_empty() {
        let conn = open_migrated();
        insert_note(&conn, "n1", "Something", "Unrelated content.");

        let hits = search_fts(&conn, "zzzyyyxxx", 10).unwrap();
        assert!(hits.is_empty());
    }

    // ── column-scoped search ──────────────────────────────────────────────────

    #[test]
    fn column_scoped_title_search() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "t1",
            "Deep Work",
            "This book is about concentration.",
        );
        insert_note(&conn, "t2", "Focus Book", "Deep techniques for working.");

        // "title:deep" should only match t1 (title contains "Deep")
        let hits = search_fts(&conn, "title:deep", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, "t1");
    }

    // ── empty query ───────────────────────────────────────────────────────────

    #[test]
    fn empty_query_returns_error() {
        let conn = open_migrated();
        let err = search_fts(&conn, "", 10).unwrap_err();
        assert!(matches!(err, FtsError::EmptyQuery));
    }

    #[test]
    fn whitespace_only_query_returns_error() {
        let conn = open_migrated();
        let err = search_fts(&conn, "   ", 10).unwrap_err();
        assert!(matches!(err, FtsError::EmptyQuery));
    }

    // ── update reflects new content ───────────────────────────────────────────

    #[test]
    fn update_note_reflected_in_fts() {
        let conn = open_migrated();
        insert_note(&conn, "upd-1", "Initial Title", "First version of content.");

        // No hit for "second"
        assert!(search_fts(&conn, "second", 10).unwrap().is_empty());

        // Update note content
        conn.execute(
            "UPDATE notes SET content = 'Second version with new content.' WHERE id = 'upd-1'",
            [],
        )
        .unwrap();

        // FTS should now find "second"
        let hits = search_fts(&conn, "second", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, "upd-1");
    }

    // ── delete reflects in FTS ────────────────────────────────────────────────

    #[test]
    fn delete_note_removed_from_fts() {
        let conn = open_migrated();
        insert_note(&conn, "del-1", "Delete Me", "Temporary note to be removed.");

        assert!(!search_fts(&conn, "temporary", 10).unwrap().is_empty());

        conn.execute("DELETE FROM notes WHERE id = 'del-1'", [])
            .unwrap();

        assert!(search_fts(&conn, "temporary", 10).unwrap().is_empty());
    }

    // ── snippet generation ────────────────────────────────────────────────────

    #[test]
    fn snippet_contains_match_markers() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "snip-1",
            "Test Note",
            "The quick brown fox jumps over the lazy dog.",
        );

        let hits = search_fts(&conn, "fox", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].snippet.contains("<b>"),
            "snippet should contain <b> markers: {}",
            hits[0].snippet
        );
    }

    // ── limit is respected ────────────────────────────────────────────────────

    #[test]
    fn limit_caps_results() {
        let conn = open_migrated();
        for i in 0..5 {
            insert_note(
                &conn,
                &format!("lim-{i}"),
                &format!("Note {i}"),
                "common content word here",
            );
        }
        let hits = search_fts(&conn, "common", 3).unwrap();
        assert!(hits.len() <= 3);
    }

    // ── rebuild_index ─────────────────────────────────────────────────────────

    #[test]
    fn rebuild_index_does_not_error() {
        let conn = open_migrated();
        insert_note(&conn, "rb-1", "Rebuild Test", "Some content.");
        rebuild_index(&conn).unwrap();
        // After rebuild, search still works.
        let hits = search_fts(&conn, "rebuild", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
