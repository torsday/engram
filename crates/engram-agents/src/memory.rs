//! Per-agent persistent key-value memory store backed by `agent_memory`.
//!
//! Each `AgentMemory` instance is bound to a single `agent_name` at
//! construction time.  All reads and writes are scoped to that name —
//! cross-agent access is syntactically impossible without obtaining a
//! second `AgentMemory` bound to the other agent, which requires an
//! explicit `agent_name` argument.
//!
//! ## TTL semantics
//!
//! Rows whose `expires_at` is `NULL` are permanent.  Rows with a non-null
//! `expires_at` in the past are *filtered on read* (treated as absent) and
//! physically removed by [`AgentMemory::expire_stale`].
//!
//! ## Capacity cap
//!
//! Each agent namespace is capped at [`AgentMemory::max_entries`] rows
//! (default 10 000).  When a `set` call would exceed the cap, the oldest
//! rows (by `created_at` ASC) are deleted first so the insertion succeeds
//! within the cap.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use thiserror::Error;

/// Default per-agent entry cap.
pub const DEFAULT_MAX_ENTRIES: u64 = 10_000;

/// Error returned by all `AgentMemory` operations.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// Underlying SQLite failure.
    #[error("agent memory SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A JSON value could not be serialized / deserialized.
    #[error("agent memory JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Per-agent persistent KV store.
///
/// All methods are synchronous and take `&self` because the `Connection` is
/// guarded by an `Arc<Mutex<…>>` — safe for sharing across threads inside
/// an agent host.
#[derive(Clone)]
pub struct AgentMemory {
    agent_name: String,
    conn: Arc<Mutex<Connection>>,
    max_entries: u64,
}

impl AgentMemory {
    /// Bind a memory store to `agent_name`, using the provided connection.
    ///
    /// `max_entries` is the per-namespace entry cap; pass
    /// [`DEFAULT_MAX_ENTRIES`] if you don't need a custom limit.
    pub fn new(agent_name: &str, conn: Arc<Mutex<Connection>>, max_entries: u64) -> Self {
        Self {
            agent_name: agent_name.to_owned(),
            conn,
            max_entries,
        }
    }

    /// Convenience constructor with the default cap.
    pub fn with_default_cap(agent_name: &str, conn: Arc<Mutex<Connection>>) -> Self {
        Self::new(agent_name, conn, DEFAULT_MAX_ENTRIES)
    }

    // ── helpers ──────────────────────────────────────────────────────────

    fn now_rfc3339() -> String {
        Utc::now().to_rfc3339()
    }

    // ── public API ───────────────────────────────────────────────────────

    /// Retrieve the JSON value for `key`, or `None` if absent or expired.
    pub fn get(&self, key: &str) -> Result<Option<Value>, MemoryError> {
        let conn = self.conn.lock().expect("agent_memory conn poisoned");
        let now = Self::now_rfc3339();
        let json_str: Option<String> = conn
            .query_row(
                "SELECT value FROM agent_memory
                  WHERE agent_name = ?1
                    AND key = ?2
                    AND (expires_at IS NULL OR expires_at > ?3)",
                params![self.agent_name, key, now],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match json_str {
            None => Ok(None),
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        }
    }

    /// Persist `value` under `key`, optionally expiring after `ttl`.
    ///
    /// Upserts (INSERT OR REPLACE) so calling `set` twice with the same key
    /// overwrites the previous entry.  If the namespace is at or above
    /// `max_entries`, the oldest rows are evicted first.
    pub fn set(&self, key: &str, value: &Value, ttl: Option<Duration>) -> Result<(), MemoryError> {
        let json = serde_json::to_string(value)?;
        let now = Self::now_rfc3339();
        let expires_at: Option<String> = ttl.map(|d| {
            let expire_time: DateTime<Utc> =
                Utc::now() + chrono::Duration::from_std(d).unwrap_or(chrono::Duration::zero());
            expire_time.to_rfc3339()
        });

        let conn = self.conn.lock().expect("agent_memory conn poisoned");

        // Count current entries for this agent (excluding the key being
        // set, which may already exist and would be replaced).
        let existing_count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM agent_memory
              WHERE agent_name = ?1 AND key != ?2",
            params![self.agent_name, key],
            |row| row.get(0),
        )?;

        // Evict oldest rows if we're at or over the cap.
        if existing_count >= self.max_entries {
            let excess = existing_count - self.max_entries + 1;
            conn.execute(
                "DELETE FROM agent_memory
                  WHERE agent_name = ?1
                    AND key IN (
                        SELECT key FROM agent_memory
                         WHERE agent_name = ?1
                         ORDER BY created_at ASC
                         LIMIT ?2
                    )",
                params![self.agent_name, excess],
            )?;
        }

        conn.execute(
            "INSERT OR REPLACE INTO agent_memory
                    (agent_name, key, value, created_at, expires_at)
                    VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.agent_name, key, json, now, expires_at],
        )?;
        Ok(())
    }

    /// Remove the entry for `key`.  A no-op if the key doesn't exist.
    pub fn delete(&self, key: &str) -> Result<(), MemoryError> {
        let conn = self.conn.lock().expect("agent_memory conn poisoned");
        conn.execute(
            "DELETE FROM agent_memory WHERE agent_name = ?1 AND key = ?2",
            params![self.agent_name, key],
        )?;
        Ok(())
    }

    /// Return all live (non-expired) `(key, value)` pairs whose key starts
    /// with `prefix`, ordered by key.
    pub fn scan(&self, prefix: &str) -> Result<Vec<(String, Value)>, MemoryError> {
        let conn = self.conn.lock().expect("agent_memory conn poisoned");
        let now = Self::now_rfc3339();
        // Use LIKE pattern: `prefix` + `%`.  Escape literal `%` and `_`.
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut stmt = conn.prepare(
            "SELECT key, value FROM agent_memory
              WHERE agent_name = ?1
                AND key LIKE ?2 ESCAPE '\\'
                AND (expires_at IS NULL OR expires_at > ?3)
              ORDER BY key",
        )?;
        let rows = stmt.query_map(params![self.agent_name, pattern, now], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (k, v_str) = row?;
            let v: Value = serde_json::from_str(&v_str).map_err(MemoryError::Json)?;
            result.push((k, v));
        }
        Ok(result)
    }

    /// Delete all rows in this agent's namespace whose `expires_at` is in
    /// the past.  Should be called at agent startup and periodically.
    pub fn expire_stale(&self) -> Result<u64, MemoryError> {
        let conn = self.conn.lock().expect("agent_memory conn poisoned");
        let now = Self::now_rfc3339();
        let n = conn.execute(
            "DELETE FROM agent_memory
              WHERE agent_name = ?1
                AND expires_at IS NOT NULL
                AND expires_at <= ?2",
            params![self.agent_name, now],
        )?;
        Ok(n as u64)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn setup() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().expect("in-memory DB");
        conn.execute_batch(
            "CREATE TABLE agent_memory (
                agent_name  TEXT NOT NULL,
                key         TEXT NOT NULL,
                value       TEXT,
                created_at  TEXT NOT NULL,
                expires_at  TEXT,
                PRIMARY KEY (agent_name, key)
            );",
        )
        .expect("create table");
        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn set_and_get_roundtrip() {
        let conn = setup();
        let mem = AgentMemory::with_default_cap("linker", conn);
        mem.set("k1", &json!({"score": 0.9}), None).unwrap();
        let v = mem.get("k1").unwrap().expect("present");
        assert_eq!(v["score"], 0.9);
    }

    #[test]
    fn get_missing_key_returns_none() {
        let conn = setup();
        let mem = AgentMemory::with_default_cap("linker", conn);
        assert!(mem.get("no_such_key").unwrap().is_none());
    }

    #[test]
    fn delete_removes_entry() {
        let conn = setup();
        let mem = AgentMemory::with_default_cap("linker", conn);
        mem.set("k", &json!(1), None).unwrap();
        mem.delete("k").unwrap();
        assert!(mem.get("k").unwrap().is_none());
    }

    #[test]
    fn expired_entry_treated_as_absent() {
        let conn = setup();
        let mem = AgentMemory::with_default_cap("linker", conn);
        // Write an entry that expires immediately (1 ns TTL).
        mem.set("k", &json!("bye"), Some(Duration::from_nanos(1)))
            .unwrap();
        // Sleep briefly to guarantee wall-clock has advanced.
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            mem.get("k").unwrap().is_none(),
            "expired entry must not be returned"
        );
    }

    #[test]
    fn expire_stale_purges_expired_rows() {
        let conn = setup();
        let mem = AgentMemory::with_default_cap("linker", conn);
        mem.set("k", &json!("bye"), Some(Duration::from_nanos(1)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let n = mem.expire_stale().unwrap();
        assert_eq!(n, 1, "one expired row removed");
        // Confirm the row is gone from storage entirely.
        let raw_count: u64 = mem
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM agent_memory WHERE agent_name='linker' AND key='k'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw_count, 0);
    }

    #[test]
    fn namespace_isolation() {
        let conn = setup();
        let linker = AgentMemory::with_default_cap("linker", Arc::clone(&conn));
        let gardener = AgentMemory::with_default_cap("gardener", Arc::clone(&conn));
        linker
            .set("shared_key", &json!("linker_val"), None)
            .unwrap();
        gardener
            .set("shared_key", &json!("gardener_val"), None)
            .unwrap();
        assert_eq!(
            linker.get("shared_key").unwrap().unwrap(),
            json!("linker_val")
        );
        assert_eq!(
            gardener.get("shared_key").unwrap().unwrap(),
            json!("gardener_val")
        );
    }

    #[test]
    fn prefix_scan_returns_matching_keys() {
        let conn = setup();
        let mem = AgentMemory::with_default_cap("linker", conn);
        mem.set("rejected:a:b", &json!(true), None).unwrap();
        mem.set("rejected:c:d", &json!(false), None).unwrap();
        mem.set("accepted:x", &json!(1), None).unwrap();
        let results = mem.scan("rejected:").unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|(k, _)| k == "rejected:a:b"));
        assert!(results.iter().any(|(k, _)| k == "rejected:c:d"));
    }

    #[test]
    fn eviction_at_cap() {
        let conn = setup();
        let mem = AgentMemory::new("linker", conn, 3);
        mem.set("k1", &json!(1), None).unwrap();
        mem.set("k2", &json!(2), None).unwrap();
        mem.set("k3", &json!(3), None).unwrap();
        // k4 insert must evict the oldest (k1).
        mem.set("k4", &json!(4), None).unwrap();
        let all = mem.scan("k").unwrap();
        assert_eq!(all.len(), 3, "cap enforced: still 3 entries");
        let keys: Vec<&str> = all.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.contains(&"k1"), "oldest evicted");
        assert!(keys.contains(&"k4"), "new entry present");
    }
}
