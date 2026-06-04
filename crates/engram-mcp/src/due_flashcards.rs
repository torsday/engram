//! `due_flashcards` MCP tool — Tutor's cards that are due for review.
//!
//! Returns the flashcards whose FSRS schedule has come due (or that have never
//! been reviewed), so a client can start a practice session in-conversation or
//! answer "what's due?".
//!
//! A translation surface over the index: reads the `flashcards` table. The only
//! logic is the due filter, the optional deck filter, and deriving
//! `interval_days` from the schedule.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "deck":  "ml",   // optional: source-note path prefix (folder-as-deck)
//!   "limit": 20      // optional, default 20
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "cards": [
//!     {
//!       "id": "01J…", "front": "Q?", "back": "A",
//!       "source_note_id": "01J…", "scheduled_for": "2024-06-01T00:00:00Z",
//!       "interval_days": 6, "reps": 3
//!     }
//!   ]
//! }
//! ```
//!
//! A card is "due" when its `next_review_at` is in the past, or when it has
//! never been scheduled (a new card). New cards sort first, then the most
//! overdue. `deck` is the source note's path prefix — engram has no separate
//! deck model yet, so a folder of source notes is the natural grouping.
//!
//! ## Error codes
//!
//! | code                   | meaning                                   |
//! |------------------------|-------------------------------------------|
//! | `bad_input`            | `limit == 0`                              |
//! | `vault_not_configured` | SQLite DB not found / not accessible      |
//! | `internal_error`       | Unexpected SQLite failure                 |

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, ToSql};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for `due_flashcards`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DueFlashcardsInput {
    /// Restrict to cards whose source note path starts with this prefix
    /// (folder-as-deck). `None` returns due cards from any deck.
    pub deck: Option<String>,
    /// Max cards to return (default 20).
    pub limit: usize,
}

impl Default for DueFlashcardsInput {
    /// `Default` matches the serde default (`limit = 20`) — the server uses it
    /// for a null/absent `arguments` payload and `limit == 0` is rejected.
    fn default() -> Self {
        Self {
            deck: None,
            limit: 20,
        }
    }
}

/// A single due flashcard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DueCard {
    pub id: String,
    /// The prompt side (the card's question).
    pub front: String,
    /// The answer side.
    pub back: Option<String>,
    /// The note this card was generated from.
    pub source_note_id: String,
    /// ISO-8601 time the card is scheduled for; `None` for a never-reviewed
    /// (new) card.
    pub scheduled_for: Option<String>,
    /// The current scheduling interval in days (from the schedule, falling
    /// back to FSRS stability). `0` for a new card.
    pub interval_days: i64,
    /// Number of times the card has been reviewed.
    pub reps: i64,
}

/// Output for `due_flashcards`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DueFlashcardsOutput {
    pub cards: Vec<DueCard>,
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

/// Raw row pulled from the `flashcards` table.
struct Row {
    id: String,
    front: String,
    back: String,
    source_note_id: String,
    next_review_at: Option<String>,
    last_review_at: Option<String>,
    stability: Option<f64>,
    reps: i64,
}

/// List the due flashcards.
pub fn handle(
    vault_root: &Path,
    input: DueFlashcardsInput,
) -> Result<DueFlashcardsOutput, ToolError> {
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

    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(now)];

    // Due = never scheduled OR scheduled in the past. New cards (NULL) sort
    // first via the `next_review_at IS NOT NULL` key, then most-overdue first.
    let deck_join_filter = if let Some(deck) = &input.deck {
        params.push(Box::new(format!("{deck}%")));
        // `?2` (not anonymous `?`) — mixing `?` with the `?1` below would make
        // SQLite assign both the same index.
        "JOIN notes n ON n.id = f.note_id AND n.path LIKE ?2"
    } else {
        ""
    };

    let sql = format!(
        "SELECT f.id, f.question, f.answer, f.note_id, f.next_review_at, \
                f.last_review_at, f.stability, f.review_count \
         FROM flashcards f {deck_join_filter} \
         WHERE (f.next_review_at IS NULL OR f.next_review_at <= ?1) \
         ORDER BY f.next_review_at IS NOT NULL, f.next_review_at ASC \
         LIMIT {}",
        input.limit
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| ToolError::internal_error(format!("prepare: {e}")))?;
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(Row {
                id: r.get(0)?,
                front: r.get(1)?,
                back: r.get(2)?,
                source_note_id: r.get(3)?,
                next_review_at: r.get(4)?,
                last_review_at: r.get(5)?,
                stability: r.get(6)?,
                reps: r.get(7)?,
            })
        })
        .map_err(|e| ToolError::internal_error(format!("query: {e}")))?;

    let cards = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| ToolError::internal_error(format!("row: {e}")))?
        .into_iter()
        .map(|row| DueCard {
            interval_days: interval_days(
                row.last_review_at.as_deref(),
                row.next_review_at.as_deref(),
                row.stability,
            ),
            id: row.id,
            front: row.front,
            back: Some(row.back),
            source_note_id: row.source_note_id,
            scheduled_for: row.next_review_at,
            reps: row.reps,
        })
        .collect();

    Ok(DueFlashcardsOutput { cards })
}

/// The scheduling interval in days: the gap between the last and next review
/// when both are known, else the FSRS `stability` rounded, else `0`.
fn interval_days(last: Option<&str>, next: Option<&str>, stability: Option<f64>) -> i64 {
    if let (Some(last), Some(next)) = (last, next) {
        if let (Ok(l), Ok(n)) = (
            DateTime::parse_from_rfc3339(last),
            DateTime::parse_from_rfc3339(next),
        ) {
            return (n - l).num_days();
        }
    }
    stability.map(|s| s.round() as i64).unwrap_or(0)
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
            for (id, path) in [("n-ml", "ml/intro.md"), ("n-cook", "cooking/pasta.md")] {
                conn.execute(
                    "INSERT INTO notes (id, path, title, note_type, content, created_by) \
                     VALUES (?1, ?2, 'N', 'evergreen', 'x', 'human')",
                    rusqlite::params![id, path],
                )
                .unwrap();
            }
            // due (overdue), new (never reviewed), and not-due (future).
            let cards = [
                (
                    "c-due",
                    "n-ml",
                    "What is RRF?",
                    "Reciprocal rank fusion.",
                    Some("2024-01-01T00:00:00Z"),
                    Some("2024-02-01T00:00:00Z"),
                    Some(31.0_f64),
                    3,
                ),
                (
                    "c-new",
                    "n-ml",
                    "What is BM25?",
                    "A ranking function.",
                    None,
                    None,
                    None,
                    0,
                ),
                (
                    "c-future",
                    "n-cook",
                    "Al dente?",
                    "Firm to the bite.",
                    Some("2024-01-01T00:00:00Z"),
                    Some("2999-01-01T00:00:00Z"),
                    Some(900.0),
                    5,
                ),
            ];
            for (id, note, q, a, last, next, stab, reps) in cards {
                conn.execute(
                    "INSERT INTO flashcards (id, note_id, question, answer, created_at, last_review_at, next_review_at, stability, review_count) \
                     VALUES (?1, ?2, ?3, ?4, '2024-01-01T00:00:00Z', ?5, ?6, ?7, ?8)",
                    rusqlite::params![id, note, q, a, last, next, stab, reps],
                )
                .unwrap();
            }
        }
        dir
    }

    fn due(dir: &Path, deck: Option<&str>) -> DueFlashcardsOutput {
        handle(
            dir,
            DueFlashcardsInput {
                deck: deck.map(String::from),
                limit: 20,
            },
        )
        .expect("due")
    }

    #[test]
    fn returns_due_and_new_excludes_future() {
        let out = due(setup_vault(true).path(), None);
        let ids: Vec<&str> = out.cards.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"c-due"));
        assert!(ids.contains(&"c-new"));
        assert!(!ids.contains(&"c-future"), "future cards are not due");
    }

    #[test]
    fn new_card_sorts_first_and_has_zero_interval() {
        let out = due(setup_vault(true).path(), None);
        assert_eq!(out.cards.first().unwrap().id, "c-new", "new cards first");
        let new = &out.cards[0];
        assert!(new.scheduled_for.is_none());
        assert_eq!(new.interval_days, 0);
        assert_eq!(new.reps, 0);
    }

    #[test]
    fn due_card_interval_from_schedule_gap() {
        let out = due(setup_vault(true).path(), None);
        let card = out.cards.iter().find(|c| c.id == "c-due").unwrap();
        // 2024-01-01 → 2024-02-01 is 31 days.
        assert_eq!(card.interval_days, 31);
        assert_eq!(card.back.as_deref(), Some("Reciprocal rank fusion."));
    }

    #[test]
    fn deck_filters_by_source_note_path_prefix() {
        let out = due(setup_vault(true).path(), Some("ml/"));
        assert!(out.cards.iter().all(|c| c.source_note_id == "n-ml"));
        assert!(out.cards.iter().any(|c| c.id == "c-due"));
    }

    #[test]
    fn limit_zero_is_bad_input() {
        let dir = setup_vault(true);
        let err = handle(
            dir.path(),
            DueFlashcardsInput {
                deck: None,
                limit: 0,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn missing_db_is_vault_not_configured() {
        let dir = TempDir::new().unwrap();
        let err = handle(dir.path(), DueFlashcardsInput::default()).unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }
}
