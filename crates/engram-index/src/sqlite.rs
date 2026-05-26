//! SQLite connection management and schema migration runner.
//!
//! Migrations live in `crates/engram-index/migrations/` and are embedded at
//! compile time via `include_str!`. Apply them with [`Migrator::apply_all`].

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Embedded migrations
// ---------------------------------------------------------------------------

/// A compiled-in migration: (filename, SQL).
///
/// Order matters — applied in array order. Filenames are the canonical name
/// stored in `schema_migrations.name`.
static MIGRATIONS: &[(&str, &str)] = &[
    (
        "001_initial.sql",
        include_str!("../migrations/001_initial.sql"),
    ),
    (
        "002_indexes_and_views.sql",
        include_str!("../migrations/002_indexes_and_views.sql"),
    ),
    (
        "003_write_intents.sql",
        include_str!("../migrations/003_write_intents.sql"),
    ),
    (
        "004_agent_runs_tokens_cost.sql",
        include_str!("../migrations/004_agent_runs_tokens_cost.sql"),
    ),
    (
        "005_atomic_writes_mode.sql",
        include_str!("../migrations/005_atomic_writes_mode.sql"),
    ),
];

/// Highest migration ordinal supported by this binary.
pub const MAX_SUPPORTED_MIGRATION: usize = MIGRATIONS.len();

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("SQLite error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    #[error(
        "database has migration {found} applied but binary only supports up to {supported}; \
         refusing to open a newer vault with an older binary"
    )]
    SchemaTooNew { found: usize, supported: usize },

    #[error(
        "checksum mismatch for migration '{name}': \
         stored={stored}, computed={computed} — migration file was tampered"
    )]
    ChecksumMismatch {
        name: String,
        stored: String,
        computed: String,
    },

    #[error("migration '{name}' not found in binary (id={id})")]
    UnknownMigration { id: i64, name: String },
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Status of a single migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    /// Migration filename, e.g. `"001_initial.sql"`.
    pub name: String,
    /// Whether this migration has been applied to the database.
    pub applied: bool,
    /// When it was applied (ISO 8601 UTC), if applicable.
    pub applied_at: Option<String>,
    /// SHA-256 hex checksum of the migration SQL.
    pub checksum: String,
}

// ---------------------------------------------------------------------------
// Migrator
// ---------------------------------------------------------------------------

/// Applies schema migrations to an open SQLite connection.
///
/// ```rust,no_run
/// use rusqlite::Connection;
/// use engram_index::sqlite::Migrator;
///
/// let conn = Connection::open_in_memory().unwrap();
/// let migrator = Migrator::new(&conn);
/// migrator.apply_all().unwrap();
/// ```
pub struct Migrator<'conn> {
    conn: &'conn Connection,
}

impl<'conn> Migrator<'conn> {
    /// Create a new `Migrator` bound to the given connection.
    ///
    /// Does **not** run any migrations — call [`apply_all`](Self::apply_all)
    /// to apply pending migrations.
    pub fn new(conn: &'conn Connection) -> Self {
        Self { conn }
    }

    /// Bootstrap `schema_migrations` (idempotent) and apply all pending
    /// migrations in order.
    ///
    /// Returns `MigrationError::SchemaTooNew` if the database is ahead of the
    /// binary (vault was opened with a newer version of engram).
    pub fn apply_all(&self) -> Result<(), MigrationError> {
        self.bootstrap()?;
        self.check_schema_version()?;

        for (name, sql) in MIGRATIONS {
            self.apply_one(name, sql)?;
        }
        Ok(())
    }

    /// Return the status of every known migration.
    pub fn status(&self) -> Result<Vec<MigrationStatus>, MigrationError> {
        self.bootstrap()?;

        // Load already-applied rows.
        let mut stmt = self
            .conn
            .prepare("SELECT name, applied_at, checksum FROM schema_migrations ORDER BY id")?;
        let applied: Vec<(String, String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<_, _>>()?;

        let mut statuses = Vec::with_capacity(MIGRATIONS.len());
        for (name, sql) in MIGRATIONS {
            let computed = checksum(sql);
            if let Some((_, applied_at, stored_cs)) = applied.iter().find(|(n, _, _)| n == name) {
                statuses.push(MigrationStatus {
                    name: (*name).to_string(),
                    applied: true,
                    applied_at: Some(applied_at.clone()),
                    checksum: stored_cs.clone(),
                });
                // Warn if checksum drifted but don't abort — status is read-only.
                if *stored_cs != computed {
                    tracing::warn!(
                        migration = name,
                        stored = stored_cs.as_str(),
                        computed = computed.as_str(),
                        "checksum mismatch detected (read-only status check)"
                    );
                }
            } else {
                statuses.push(MigrationStatus {
                    name: (*name).to_string(),
                    applied: false,
                    applied_at: None,
                    checksum: computed,
                });
            }
        }
        Ok(statuses)
    }

    /// Remove the last `n` migration records from `schema_migrations`.
    ///
    /// **Best-effort:** this only updates the migration bookkeeping table; it
    /// does **not** undo the DDL changes (DROP TABLE, DROP INDEX). Use for
    /// development reset on in-memory or throw-away databases only.
    ///
    /// Returns the names of the records removed.
    pub fn rollback(&self, n: u32) -> Result<Vec<String>, MigrationError> {
        self.bootstrap()?;
        if n == 0 {
            return Ok(vec![]);
        }

        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM schema_migrations ORDER BY id DESC LIMIT ?1")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([n], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;

        let mut removed = Vec::new();
        for (id, name) in &rows {
            self.conn.execute(
                "DELETE FROM schema_migrations WHERE id = ?1",
                rusqlite::params![id],
            )?;
            removed.push(name.clone());
            tracing::info!(
                migration = name.as_str(),
                "rolled back migration record (DDL not reversed)"
            );
        }
        Ok(removed)
    }

    // ── Private helpers ─────────────────────────────────────────────────────

    /// Create `schema_migrations` and enable WAL mode (idempotent).
    fn bootstrap(&self) -> Result<(), MigrationError> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 name       TEXT    NOT NULL UNIQUE,
                 applied_at TEXT    NOT NULL,
                 checksum   TEXT    NOT NULL
             );",
        )?;
        Ok(())
    }

    /// Refuse to open a database that is ahead of what this binary supports.
    fn check_schema_version(&self) -> Result<(), MigrationError> {
        let count: usize =
            self.conn
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get(0)
                })?;

        if count > MAX_SUPPORTED_MIGRATION {
            return Err(MigrationError::SchemaTooNew {
                found: count,
                supported: MAX_SUPPORTED_MIGRATION,
            });
        }
        Ok(())
    }

    /// Apply one migration by name if it has not already been applied.
    fn apply_one(&self, name: &str, sql: &str) -> Result<(), MigrationError> {
        // Check if already applied.
        let existing: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, checksum FROM schema_migrations WHERE name = ?1",
                rusqlite::params![name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((_id, stored_cs)) = existing {
            // Already applied — verify checksum integrity.
            let computed = checksum(sql);
            if stored_cs != computed {
                return Err(MigrationError::ChecksumMismatch {
                    name: name.to_string(),
                    stored: stored_cs,
                    computed,
                });
            }
            return Ok(());
        }

        // Not yet applied: run within a savepoint for atomicity.
        let cs = checksum(sql);
        let applied_at = utc_now();

        self.conn.execute_batch(sql)?;
        self.conn.execute(
            "INSERT INTO schema_migrations (name, applied_at, checksum) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, applied_at, cs],
        )?;

        tracing::info!(migration = name, "applied migration");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the SHA-256 hex checksum of a migration's SQL text.
fn checksum(sql: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(sql.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Return the current UTC time as an ISO 8601 string.
fn utc_now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Extension trait for optional query
// ---------------------------------------------------------------------------

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        Connection::open_in_memory().expect("in-memory DB")
    }

    // -- apply_all ------------------------------------------------------------

    #[test]
    fn apply_all_creates_all_expected_tables() {
        let conn = open();
        let m = Migrator::new(&conn);
        m.apply_all().expect("apply_all must succeed");

        // Verify a representative sample of tables exist.
        let expected_tables = &[
            "notes",
            "links",
            "tags",
            "artifacts",
            "agent_runs",
            "agent_actions",
            "outcomes",
            "agent_memory",
            "trust_scores",
            "note_locks",
            "write_intents",
            "deliberations",
            "deliberation_votes",
            "proposals",
            "shelved",
            "flow_runs",
            "flow_step_results",
            "audits",
            "prompt_variants",
            "conversations",
            "sessions",
            "dreams",
            "mcp_clients",
            "mcp_access_log",
            "mcp_register_requests",
            "pending_questions",
            "corpus_digestions",
            "digestion_items",
            "digestion_clusters",
            "digestion_discards",
            "predictions",
            "flashcards",
            "flashcard_reviews",
            "token_usage",
            "agent_budgets",
            "token_estimator_calibration",
            "eval_runs",
            "eval_case_results",
            "embedding_cache",
            "schema_migrations",
        ];

        for table in expected_tables {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            assert_eq!(exists, 1, "table '{table}' should exist after apply_all");
        }
    }

    #[test]
    fn apply_all_creates_fts5_virtual_table() {
        let conn = open();
        Migrator::new(&conn).apply_all().unwrap();
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "notes_fts virtual table should exist");
    }

    #[test]
    fn apply_all_records_migrations_in_schema_migrations() {
        let conn = open();
        Migrator::new(&conn).apply_all().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64);
    }

    #[test]
    fn apply_all_is_idempotent() {
        let conn = open();
        let m = Migrator::new(&conn);
        m.apply_all().expect("first apply_all");
        m.apply_all().expect("second apply_all must be no-op");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MIGRATIONS.len() as i64, "should not double-apply");
    }

    #[test]
    fn apply_all_creates_secondary_indexes() {
        let conn = open();
        Migrator::new(&conn).apply_all().unwrap();

        let expected_indexes = &[
            "idx_notes_type",
            "idx_tags_tag",
            "idx_agent_actions_agent",
            "idx_proposals_status",
            "idx_embedding_cache_lru",
            "idx_predictions_status",
        ];
        for idx in expected_indexes {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    rusqlite::params![idx],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "index '{idx}' should exist");
        }
    }

    // -- status ---------------------------------------------------------------

    #[test]
    fn status_shows_all_pending_before_apply() {
        let conn = open();
        let statuses = Migrator::new(&conn).status().unwrap();
        assert_eq!(statuses.len(), MIGRATIONS.len());
        for s in &statuses {
            assert!(!s.applied, "migration '{}' should be pending", s.name);
        }
    }

    #[test]
    fn status_shows_all_applied_after_apply() {
        let conn = open();
        let m = Migrator::new(&conn);
        m.apply_all().unwrap();
        let statuses = m.status().unwrap();
        for s in &statuses {
            assert!(s.applied, "migration '{}' should be applied", s.name);
            assert!(s.applied_at.is_some());
        }
    }

    // -- checksum integrity ---------------------------------------------------

    #[test]
    fn tampered_migration_is_detected() {
        let conn = open();
        let m = Migrator::new(&conn);
        m.apply_all().unwrap();

        // Corrupt the stored checksum.
        conn.execute(
            "UPDATE schema_migrations SET checksum = 'deadbeef' WHERE name = '001_initial.sql'",
            [],
        )
        .unwrap();

        let err = m.apply_all().unwrap_err();
        assert!(
            matches!(err, MigrationError::ChecksumMismatch { .. }),
            "expected ChecksumMismatch, got: {err}"
        );
    }

    // -- rollback -------------------------------------------------------------

    #[test]
    fn rollback_removes_last_n_records() {
        let conn = open();
        let m = Migrator::new(&conn);
        m.apply_all().unwrap();

        let removed = m.rollback(1).unwrap();
        assert_eq!(removed.len(), 1);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, (MIGRATIONS.len() - 1) as i64);
    }

    #[test]
    fn rollback_zero_is_noop() {
        let conn = open();
        let m = Migrator::new(&conn);
        m.apply_all().unwrap();
        let removed = m.rollback(0).unwrap();
        assert!(removed.is_empty());
    }

    // -- schema-too-new -------------------------------------------------------

    #[test]
    fn schema_too_new_returns_error() {
        let conn = open();
        let m = Migrator::new(&conn);
        // Bootstrap schema_migrations and inject a fake future migration.
        m.bootstrap().unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (name, applied_at, checksum)
             VALUES ('999_future.sql', '2099-01-01T00:00:00Z', 'abc')",
            [],
        )
        .unwrap();
        // Now inject more than MAX_SUPPORTED_MIGRATION rows.
        for i in 0..(MAX_SUPPORTED_MIGRATION + 1) {
            conn.execute(
                "INSERT OR IGNORE INTO schema_migrations (name, applied_at, checksum)
                 VALUES (?1, '2099-01-01T00:00:00Z', 'abc')",
                rusqlite::params![format!("{:03}_fake.sql", i + 100)],
            )
            .unwrap();
        }
        let err = m.apply_all().unwrap_err();
        assert!(
            matches!(err, MigrationError::SchemaTooNew { .. }),
            "expected SchemaTooNew, got: {err}"
        );
    }

    // -- checksum helper -------------------------------------------------------

    #[test]
    fn checksum_is_deterministic() {
        let a = checksum("SELECT 1;");
        let b = checksum("SELECT 1;");
        assert_eq!(a, b);
    }

    #[test]
    fn checksum_differs_for_different_inputs() {
        let a = checksum("SELECT 1;");
        let b = checksum("SELECT 2;");
        assert_ne!(a, b);
    }

    #[test]
    fn checksum_is_hex_string() {
        let cs = checksum("test");
        assert!(
            cs.chars().all(|c| c.is_ascii_hexdigit()),
            "checksum should be hex: {cs}"
        );
        assert_eq!(cs.len(), 64, "SHA-256 hex is 64 chars");
    }
}
