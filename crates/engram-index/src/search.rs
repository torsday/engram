//! Hybrid retrieval: BM25 (FTS5) + Reciprocal Rank Fusion.
//!
//! The public entry point is [`hybrid_search`]. It runs BM25 and applies
//! optional metadata filters, returning ranked [`SearchResult`]s fused
//! with RRF scoring.
//!
//! ## Dense ANN retrieval
//!
//! The ANN path is a stub in v1 — the vector store is operational but
//! requires an embedding vector for the query, which belongs in the
//! embedding pipeline (a follow-up issue). When the embedding pipeline
//! lands it will populate `provenance: "dense"` or `"both"` on results
//! that appear in ANN output; for now all results carry `"bm25"`.
//!
//! ## Cross-encoder rerank + graph expansion
//!
//! Deferred to follow-up issues — not in v1 scope.

use std::collections::HashMap;

use rusqlite::{types::ToSql, Connection};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors from hybrid search.
#[derive(Debug, Error)]
pub enum SearchError {
    /// SQLite / FTS5 error.
    #[error("SQLite: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    /// The query was empty.
    #[error("search query must not be empty")]
    EmptyQuery,
}

/// Optional metadata filters narrowing the result set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilter {
    /// Only return notes with this tag.
    pub tag: Option<String>,
    /// Only return notes of this `note_type` (e.g. `"evergreen"`, `"fleeting"`).
    pub note_type: Option<String>,
    /// Only return notes modified at or after this ISO-8601 timestamp.
    pub since: Option<String>,
    /// Only return notes created by this author (human name or agent name).
    pub author: Option<String>,
}

/// Provenance of a result in the fused ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Appeared only in BM25 results.
    Bm25,
    /// Appeared only in ANN results.
    Dense,
    /// Appeared in both BM25 and ANN results.
    Both,
}

/// One ranked result from hybrid search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// ULID of the matched note.
    pub note_id: String,
    /// Note title.
    pub title: String,
    /// Path of the note relative to the vault root.
    pub path: String,
    /// Short excerpt from the note body (FTS5 snippet with `<b>` markers).
    pub snippet: String,
    /// Fused relevance score (higher = more relevant).
    pub score: f64,
    /// Which retrieval method(s) produced this result.
    pub provenance: Provenance,
}

// ---------------------------------------------------------------------------
// RRF
// ---------------------------------------------------------------------------

/// Reciprocal Rank Fusion constant (Cormack et al., standard value).
const RRF_K: f64 = 60.0;

fn rrf_score(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run hybrid search over the notes in `conn` (a migrated `engram.db`).
///
/// Returns up to `k` results ordered by descending fused score.
///
/// # Errors
///
/// Returns [`SearchError::EmptyQuery`] for blank queries, or
/// [`SearchError::Rusqlite`] on any SQLite failure.
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    k: usize,
    filter: &SearchFilter,
) -> Result<Vec<SearchResult>, SearchError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(SearchError::EmptyQuery);
    }

    let bm25_hits = bm25_search(conn, trimmed, k * 3, filter)?;

    // RRF accumulator: note_id → (accumulated_score, from_bm25, from_ann)
    let mut scores: HashMap<String, (f64, bool, bool)> = HashMap::new();
    for (rank, hit) in bm25_hits.iter().enumerate() {
        let e = scores
            .entry(hit.note_id.clone())
            .or_insert((0.0, false, false));
        e.0 += rrf_score(rank);
        e.1 = true;
    }

    let meta: HashMap<&str, &RawHit> = bm25_hits.iter().map(|h| (h.note_id.as_str(), h)).collect();

    let mut results: Vec<SearchResult> = scores
        .into_iter()
        .filter_map(|(note_id, (score, from_bm25, from_ann))| {
            let provenance = match (from_bm25, from_ann) {
                (true, true) => Provenance::Both,
                (true, false) => Provenance::Bm25,
                (false, true) => Provenance::Dense,
                _ => return None,
            };
            let hit = meta.get(note_id.as_str())?;
            Some(SearchResult {
                note_id,
                title: hit.title.clone(),
                path: hit.path.clone(),
                snippet: hit.snippet.clone(),
                score,
                provenance,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(k);
    Ok(results)
}

// ---------------------------------------------------------------------------
// BM25 (FTS5) layer
// ---------------------------------------------------------------------------

struct RawHit {
    note_id: String,
    title: String,
    path: String,
    snippet: String,
}

fn bm25_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    filter: &SearchFilter,
) -> Result<Vec<RawHit>, SearchError> {
    // Build dynamic WHERE clause. ?1 is always the FTS query; optional
    // filter params start at ?2 and are appended in the order below.
    let mut extra_clauses: Vec<String> = Vec::new();
    let mut extra_values: Vec<Box<dyn ToSql>> = Vec::new();
    let mut next_param = 2usize;

    if let Some(ref v) = filter.tag {
        extra_clauses.push(format!(
            "EXISTS (SELECT 1 FROM tags WHERE tags.note_id = n.id AND tags.tag = ?{next_param})"
        ));
        extra_values.push(Box::new(v.clone()));
        next_param += 1;
    }
    if let Some(ref v) = filter.note_type {
        extra_clauses.push(format!("n.note_type = ?{next_param}"));
        extra_values.push(Box::new(v.clone()));
        next_param += 1;
    }
    if let Some(ref v) = filter.since {
        extra_clauses.push(format!("n.modified_at >= ?{next_param}"));
        extra_values.push(Box::new(v.clone()));
        next_param += 1;
    }
    if let Some(ref v) = filter.author {
        extra_clauses.push(format!("n.created_by = ?{next_param}"));
        extra_values.push(Box::new(v.clone()));
        next_param += 1;
    }

    let limit_param = next_param;
    let mut where_parts = vec!["notes_fts MATCH ?1".to_string()];
    where_parts.extend(extra_clauses);
    let where_sql = where_parts.join(" AND ");

    let sql = format!(
        "SELECT n.id, n.title, n.path, \
                snippet(notes_fts, 1, '<b>', '</b>', '…', 10) \
         FROM notes_fts \
         JOIN notes n ON notes_fts.rowid = n.rowid \
         WHERE {where_sql} \
         ORDER BY bm25(notes_fts) \
         LIMIT ?{limit_param}"
    );

    let mut stmt = conn.prepare(&sql)?;

    // Bind FTS query (param 1)
    stmt.raw_bind_parameter(1, query)?;
    // Bind extra filter params
    for (i, val) in extra_values.iter().enumerate() {
        stmt.raw_bind_parameter(i + 2, val.as_ref())?;
    }
    // Bind limit
    stmt.raw_bind_parameter(limit_param, limit as i64)?;

    let mut rows = stmt.raw_query();
    let mut hits = Vec::new();
    while let Some(row) = rows.next()? {
        hits.push(RawHit {
            note_id: row.get(0)?,
            title: row.get(1)?,
            path: row.get(2)?,
            snippet: row.get(3)?,
        });
    }
    Ok(hits)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::Migrator;
    use rusqlite::{params, Connection};

    fn open_migrated() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        conn
    }

    fn insert_note(conn: &Connection, id: &str, title: &str, body: &str, note_type: &str) {
        conn.execute(
            "INSERT INTO notes \
             (id, path, title, note_type, content, modified_at, created_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, '2024-01-01T00:00:00Z', 'human')",
            params![id, format!("{id}.md"), title, note_type, body],
        )
        .unwrap();
    }

    fn insert_tag(conn: &Connection, note_id: &str, tag: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO tags (note_id, tag) VALUES (?1, ?2)",
            params![note_id, tag],
        )
        .unwrap();
    }

    #[test]
    fn basic_bm25_hit() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "n1",
            "Atomic Habits",
            "Small changes compound over time.",
            "evergreen",
        );
        insert_note(
            &conn,
            "n2",
            "Deep Work",
            "Focus is the new currency.",
            "evergreen",
        );

        let results = hybrid_search(&conn, "compound", 10, &SearchFilter::default()).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].note_id, "n1");
        assert!(results[0].score > 0.0);
        assert_eq!(results[0].provenance, Provenance::Bm25);
    }

    #[test]
    fn no_match_returns_empty() {
        let conn = open_migrated();
        insert_note(&conn, "n1", "Title", "Body content.", "evergreen");
        let results = hybrid_search(&conn, "zzzyyyxxx", 10, &SearchFilter::default()).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn empty_query_returns_error() {
        let conn = open_migrated();
        let err = hybrid_search(&conn, "", 10, &SearchFilter::default()).unwrap_err();
        assert!(matches!(err, SearchError::EmptyQuery));
    }

    #[test]
    fn whitespace_query_returns_error() {
        let conn = open_migrated();
        let err = hybrid_search(&conn, "   ", 10, &SearchFilter::default()).unwrap_err();
        assert!(matches!(err, SearchError::EmptyQuery));
    }

    #[test]
    fn filter_by_note_type() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "e1",
            "Evergreen Note",
            "compound interest principle",
            "evergreen",
        );
        insert_note(
            &conn,
            "f1",
            "Fleeting Note",
            "compound idea today",
            "fleeting",
        );

        let filter = SearchFilter {
            note_type: Some("evergreen".into()),
            ..Default::default()
        };
        let results = hybrid_search(&conn, "compound", 10, &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].note_id, "e1");
    }

    #[test]
    fn filter_by_tag() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "t1",
            "Tagged Note",
            "compound systems thinking",
            "evergreen",
        );
        insert_note(
            &conn,
            "t2",
            "Untagged Note",
            "compound unrelated content",
            "evergreen",
        );
        insert_tag(&conn, "t1", "systems");

        let filter = SearchFilter {
            tag: Some("systems".into()),
            ..Default::default()
        };
        let results = hybrid_search(&conn, "compound", 10, &filter).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].note_id, "t1");
    }

    #[test]
    fn results_descending_by_score() {
        let conn = open_migrated();
        for i in 0..5 {
            insert_note(
                &conn,
                &format!("note-{i}"),
                &format!("Learning Note {i}"),
                "learning is a lifelong process worth investing in deeply",
                "evergreen",
            );
        }
        let results = hybrid_search(&conn, "learning", 10, &SearchFilter::default()).unwrap();
        let scores: Vec<f64> = results.iter().map(|r| r.score).collect();
        let mut sorted = scores.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(scores, sorted);
    }

    #[test]
    fn k_limit_respected() {
        let conn = open_migrated();
        for i in 0..10 {
            insert_note(
                &conn,
                &format!("lim-{i}"),
                &format!("Limit Note {i}"),
                "common keyword here for search testing",
                "evergreen",
            );
        }
        let results = hybrid_search(&conn, "common", 3, &SearchFilter::default()).unwrap();
        assert!(results.len() <= 3);
    }

    #[test]
    fn result_fields_populated() {
        let conn = open_migrated();
        insert_note(
            &conn,
            "fld-1",
            "Field Test",
            "The answer is forty-two.",
            "evergreen",
        );
        let results = hybrid_search(&conn, "forty", 5, &SearchFilter::default()).unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.note_id, "fld-1");
        assert_eq!(r.title, "Field Test");
        assert_eq!(r.path, "fld-1.md");
        assert!(!r.snippet.is_empty());
    }

    #[test]
    fn rrf_score_positive_and_decreasing() {
        assert!(rrf_score(0) > 0.0);
        assert!(rrf_score(0) > rrf_score(1));
        assert!(rrf_score(1) > rrf_score(10));
    }
}
