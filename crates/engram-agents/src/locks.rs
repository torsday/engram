//! Per-note advisory lock manager.
//!
//! Prevents two agents from modifying the same note concurrently. Uses the
//! `note_locks` SQLite table (defined in migration 001) as the lock store.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use engram_agents::locks::{LockManager, LockConfig};
//! use rusqlite::Connection;
//! use std::sync::Arc;
//!
//! let conn = Arc::new(std::sync::Mutex::new(Connection::open_in_memory().unwrap()));
//! let lm = LockManager::new(conn, LockConfig::default());
//! let guard = lm.acquire("note-ulid", "linker-agent", None).unwrap();
//! // guard releases lock on drop
//! ```
//!
//! ## Lock inheritance
//!
//! When a sub-agent is invoked by a parent agent that already holds a lock on a
//! note, the sub-agent passes `parent_holder` to `acquire`. The manager checks
//! whether the parent holds the lock; if so, the sub-agent receives an inherited
//! `LockGuard` without contesting the table.
//!
//! ## Expiration
//!
//! Locks expire after `LockConfig::ttl_secs` (default 300s). On every `acquire`
//! call, expired locks for the target note are reaped before trying to acquire,
//! preventing zombie locks from a panicking agent from blocking forever.

use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use rusqlite::{params, Connection};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for [`LockManager`].
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// How long a lock lives before it expires (default: 300s).
    pub ttl_secs: i64,
    /// Maximum number of acquisition retries before giving up (default: 3).
    pub max_retries: u32,
    /// Base delay between retries in milliseconds (default: 100ms).
    /// Actual delay = base + random jitter in [0, base).
    pub retry_base_ms: u64,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 300,
            max_retries: 3,
            retry_base_ms: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from lock operations.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum LockError {
    /// Lock acquisition failed after all retries.
    #[error("note `{note_id}` is locked by `{current_holder}` until {expires_at}")]
    AcquisitionFailed {
        note_id: String,
        current_holder: String,
        expires_at: String,
    },
    /// SQLite error.
    #[error("SQLite error: {0}")]
    Sqlite(String),
}

impl From<rusqlite::Error> for LockError {
    fn from(e: rusqlite::Error) -> Self {
        LockError::Sqlite(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// LockGuard
// ---------------------------------------------------------------------------

/// RAII lock guard. Releases the lock when dropped.
///
/// Dropping this guard executes a `DELETE FROM note_locks WHERE note_id = ?
/// AND locked_by = ?`. Panics inside `Drop` are suppressed — if the release
/// fails (e.g. connection gone), the lock will expire naturally via the TTL.
#[must_use = "dropping the guard releases the lock; assign it to keep the lock held"]
pub struct LockGuard {
    note_id: String,
    holder: String,
    inherited: bool,
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard")
            .field("note_id", &self.note_id)
            .field("holder", &self.holder)
            .field("inherited", &self.inherited)
            .finish()
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.inherited {
            // Inherited locks are owned by the parent; don't release them.
            return;
        }
        // Suppress errors — panicking in Drop is undefined behaviour.
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "DELETE FROM note_locks WHERE note_id = ?1 AND locked_by = ?2",
                params![self.note_id, self.holder],
            );
        }
    }
}

impl LockGuard {
    /// Note ULID that this guard protects.
    pub fn note_id(&self) -> &str {
        &self.note_id
    }

    /// Agent name that holds (or inherited) this lock.
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// Whether this guard represents an inherited lock (owned by a parent).
    pub fn is_inherited(&self) -> bool {
        self.inherited
    }
}

// ---------------------------------------------------------------------------
// LockManager
// ---------------------------------------------------------------------------

/// Advisory per-note lock manager backed by the `note_locks` SQLite table.
#[derive(Clone)]
pub struct LockManager {
    conn: Arc<Mutex<Connection>>,
    config: LockConfig,
}

impl LockManager {
    /// Create a new `LockManager` using `conn` as the lock store.
    pub fn new(conn: Arc<Mutex<Connection>>, config: LockConfig) -> Self {
        Self { conn, config }
    }

    /// Acquire an advisory lock on `note_id` for `holder`.
    ///
    /// If `parent_holder` is provided and it currently holds the lock (directly
    /// or via inheritance), returns an inherited `LockGuard` without contesting.
    ///
    /// On conflict, backs off with jitter and retries up to
    /// `config.max_retries` times before returning
    /// [`LockError::AcquisitionFailed`].
    pub fn acquire(
        &self,
        note_id: &str,
        holder: &str,
        parent_holder: Option<&str>,
    ) -> Result<LockGuard, LockError> {
        // Reap expired locks for this note first.
        self.reap_expired(note_id)?;

        // Check for parent inheritance.
        if let Some(parent) = parent_holder {
            if self.is_locked_by(note_id, parent)? {
                return Ok(LockGuard {
                    note_id: note_id.to_string(),
                    holder: holder.to_string(),
                    inherited: true,
                    conn: Arc::clone(&self.conn),
                });
            }
        }

        // Try to acquire.
        let mut attempts = 0u32;
        loop {
            match self.try_insert(note_id, holder) {
                Ok(true) => {
                    return Ok(LockGuard {
                        note_id: note_id.to_string(),
                        holder: holder.to_string(),
                        inherited: false,
                        conn: Arc::clone(&self.conn),
                    });
                }
                Ok(false) => {
                    // Another holder owns the lock.
                    attempts += 1;
                    if attempts > self.config.max_retries {
                        let (current_holder, expires_at) = self.current_lock_info(note_id)?;
                        return Err(LockError::AcquisitionFailed {
                            note_id: note_id.to_string(),
                            current_holder,
                            expires_at,
                        });
                    }
                    // Jittered back-off (no async; sync sleep for simplicity
                    // — the runner drives this from a tokio blocking task).
                    let jitter = pseudo_jitter(self.config.retry_base_ms);
                    std::thread::sleep(std::time::Duration::from_millis(
                        self.config.retry_base_ms + jitter,
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Return `true` if `agent` currently holds a live lock on `note_id`.
    pub fn is_held_by(&self, note_id: &str, agent: &str) -> Result<bool, LockError> {
        self.is_locked_by(note_id, agent)
    }

    // ── private helpers ──────────────────────────────────────────────────────

    /// Try an atomic INSERT; return `true` if we inserted, `false` on conflict.
    fn try_insert(&self, note_id: &str, holder: &str) -> Result<bool, LockError> {
        let now = Utc::now();
        let locked_at = now.to_rfc3339();
        let expires_at = (now + Duration::seconds(self.config.ttl_secs)).to_rfc3339();

        let conn = self
            .conn
            .lock()
            .map_err(|e| LockError::Sqlite(e.to_string()))?;
        let rows = conn.execute(
            "INSERT OR IGNORE INTO note_locks (note_id, locked_by, locked_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![note_id, holder, locked_at, expires_at],
        )?;
        Ok(rows > 0)
    }

    /// Reap expired lock for `note_id` (if any).
    fn reap_expired(&self, note_id: &str) -> Result<(), LockError> {
        let now = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|e| LockError::Sqlite(e.to_string()))?;
        conn.execute(
            "DELETE FROM note_locks WHERE note_id = ?1 AND expires_at < ?2",
            params![note_id, now],
        )?;
        Ok(())
    }

    /// Check whether `agent` holds a live (non-expired) lock on `note_id`.
    fn is_locked_by(&self, note_id: &str, agent: &str) -> Result<bool, LockError> {
        let now = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|e| LockError::Sqlite(e.to_string()))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM note_locks \
             WHERE note_id = ?1 AND locked_by = ?2 AND expires_at > ?3",
            params![note_id, agent, now],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Return `(current_holder, expires_at)` for the current lock on `note_id`.
    /// Falls back to sensible defaults if the row has disappeared by the time we query.
    fn current_lock_info(&self, note_id: &str) -> Result<(String, String), LockError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| LockError::Sqlite(e.to_string()))?;
        let result = conn.query_row(
            "SELECT locked_by, expires_at FROM note_locks WHERE note_id = ?1",
            params![note_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok(pair) => Ok(pair),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                Ok(("unknown".to_string(), "unknown".to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Very lightweight deterministic "jitter" — no rand crate needed here.
/// Returns a value in [0, base) derived from the current nanosecond count.
fn pseudo_jitter(base: u64) -> u64 {
    if base == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos as u64) % base
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engram_index::sqlite::Migrator;
    use rusqlite::Connection;

    fn make_lm() -> LockManager {
        let conn = Connection::open_in_memory().unwrap();
        Migrator::new(&conn).apply_all().unwrap();
        LockManager::new(
            Arc::new(Mutex::new(conn)),
            LockConfig {
                ttl_secs: 5,
                max_retries: 2,
                retry_base_ms: 1,
            },
        )
    }

    // -- we need note_locks to have notes rows (FK). Insert a stub note.
    fn insert_note(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO notes (id, path, title, note_type) VALUES (?1, ?2, ?3, 'evergreen')",
            params![id, format!("{id}.md"), format!("Note {id}")],
        )
        .unwrap();
    }

    fn setup_note(lm: &LockManager, id: &str) {
        let conn = lm.conn.lock().unwrap();
        insert_note(&conn, id);
    }

    // ── acquire ───────────────────────────────────────────────────────────────

    #[test]
    fn acquire_returns_guard() {
        let lm = make_lm();
        setup_note(&lm, "n1");
        let guard = lm.acquire("n1", "linker", None).unwrap();
        assert_eq!(guard.note_id(), "n1");
        assert_eq!(guard.holder(), "linker");
        assert!(!guard.is_inherited());
    }

    // ── drop releases lock ────────────────────────────────────────────────────

    #[test]
    fn drop_releases_lock() {
        let lm = make_lm();
        setup_note(&lm, "n2");
        {
            let _g = lm.acquire("n2", "linker", None).unwrap();
            assert!(lm.is_held_by("n2", "linker").unwrap());
        }
        assert!(!lm.is_held_by("n2", "linker").unwrap());
    }

    // ── contested ─────────────────────────────────────────────────────────────

    #[test]
    fn contested_lock_returns_error_after_retries() {
        let lm = make_lm();
        setup_note(&lm, "n3");
        let _g = lm.acquire("n3", "linker", None).unwrap();
        // Second agent tries to acquire — should fail.
        let err = lm.acquire("n3", "gardener", None).unwrap_err();
        assert!(matches!(err, LockError::AcquisitionFailed { .. }));
    }

    // ── expired lock is reaped ────────────────────────────────────────────────

    #[test]
    fn expired_lock_is_reaped_on_next_acquire() {
        let lm_short = LockManager::new(
            {
                let conn = Connection::open_in_memory().unwrap();
                Migrator::new(&conn).apply_all().unwrap();
                let id = "nexp";
                conn.execute(
                    "INSERT OR IGNORE INTO notes (id, path, title, note_type) VALUES (?1, ?2, ?3, 'evergreen')",
                    params![id, format!("{id}.md"), format!("Note {id}")],
                )
                .unwrap();
                Arc::new(Mutex::new(conn))
            },
            LockConfig {
                ttl_secs: 0, // expire immediately
                max_retries: 1,
                retry_base_ms: 1,
            },
        );

        // Insert a lock that's already expired.
        {
            let conn = lm_short.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO note_locks (note_id, locked_by, locked_at, expires_at) \
                 VALUES ('nexp', 'zombie-agent', '2020-01-01T00:00:00Z', '2020-01-01T00:00:01Z')",
                [],
            )
            .unwrap();
        }

        // New agent should successfully acquire (expired lock is reaped).
        let guard = lm_short.acquire("nexp", "linker", None).unwrap();
        assert_eq!(guard.holder(), "linker");
    }

    // ── parent inheritance ────────────────────────────────────────────────────

    #[test]
    fn sub_agent_inherits_parent_lock() {
        let lm = make_lm();
        setup_note(&lm, "n4");
        let _parent = lm.acquire("n4", "curator", None).unwrap();

        // Sub-agent acquires with parent_holder set.
        let child = lm.acquire("n4", "sub-scribe", Some("curator")).unwrap();
        assert!(child.is_inherited());
        // Dropping the child should NOT release curator's lock.
        drop(child);
        assert!(lm.is_held_by("n4", "curator").unwrap());
    }

    // ── is_held_by ────────────────────────────────────────────────────────────

    #[test]
    fn is_held_by_returns_false_for_unknown_note() {
        let lm = make_lm();
        // Note doesn't exist in DB — no lock either.
        assert!(!lm.is_held_by("no-such-note", "linker").unwrap());
    }

    // ── same agent reacquires (idempotent not required, conflict is OK) ───────

    #[test]
    fn same_agent_cannot_double_acquire() {
        let lm = make_lm();
        setup_note(&lm, "n5");
        let _g1 = lm.acquire("n5", "linker", None).unwrap();
        // Same agent trying again without parent_holder — conflicts with itself.
        let err = lm.acquire("n5", "linker", None).unwrap_err();
        assert!(matches!(err, LockError::AcquisitionFailed { .. }));
    }
}
