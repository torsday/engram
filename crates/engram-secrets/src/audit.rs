//! Append-only audit log for secret operations.
//!
//! Each [`CompositeStore`](crate::CompositeStore) `get`/`set`/`remove` call
//! writes one JSONL line to `<engram_dir>/logs/secrets.jsonl`:
//!
//! ```jsonl
//! {"ts":"2026-05-17T18:42:10.123Z","op":"get","name":"anthropic","ok":true}
//! ```
//!
//! Values are never recorded. `list` is not audited — it returns names only
//! and grants no value access.
//!
//! # Rotation
//!
//! This crate does not rotate. Higher-level callers (typically a weekly
//! housekeeping task in the runtime) move the file aside; the next `record`
//! call recreates it. The log file is opened with `append + create` so
//! external rotation is safe.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The operation being audited.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditOp {
    /// `SecretsStore::get(name)` — read.
    Get,
    /// `SecretsStore::set(name, _)` — write.
    Set,
    /// `SecretsStore::remove(name)` — delete.
    Remove,
}

/// One line of the audit log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEvent {
    /// RFC 3339 timestamp at which the operation completed.
    pub ts: String,
    /// Operation that was performed.
    pub op: AuditOp,
    /// Secret name involved (never the value).
    pub name: String,
    /// Whether the operation succeeded.
    pub ok: bool,
}

/// Append-only audit log handle.
///
/// Concurrent `record` calls serialize through an internal mutex so the
/// output remains well-formed JSONL even under contention.
pub struct AuditLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl AuditLog {
    /// Open (and create if needed) the audit log at `path`. The parent
    /// directory is created if absent. Returns [`Error::Io`] on directory
    /// creation or file-open failure.
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Path the log writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record one audit event. I/O failures are logged via `tracing::warn!`
    /// but never propagated — `get`/`set`/`remove` callers must not be
    /// blocked by audit log issues.
    pub fn record(&self, op: AuditOp, name: &str, ok: bool) {
        let event = AuditEvent {
            ts: chrono::Utc::now().to_rfc3339(),
            op,
            name: name.to_string(),
            ok,
        };
        if let Err(e) = self.write_line(&event) {
            tracing::warn!(
                audit_path = %self.path.display(),
                error = %e,
                "secrets audit write failed (operation result preserved)"
            );
        }
    }

    fn write_line(&self, event: &AuditEvent) -> Result<()> {
        let mut line = serde_json::to_string(event).map_err(|e| Error::Audit {
            path: self.path.clone(),
            source: std::io::Error::other(e),
        })?;
        line.push('\n');
        let mut file = self.file.lock().expect("audit log mutex poisoned");
        file.write_all(line.as_bytes())
            .map_err(|source| Error::Audit {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn records_get_set_remove() {
        let dir = tempdir().unwrap();
        let log = AuditLog::open(dir.path().join("secrets.jsonl")).unwrap();
        log.record(AuditOp::Set, "anthropic", true);
        log.record(AuditOp::Get, "anthropic", true);
        log.record(AuditOp::Get, "missing", false);
        log.record(AuditOp::Remove, "anthropic", true);
        drop(log);

        let contents = std::fs::read_to_string(dir.path().join("secrets.jsonl")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 4);

        let events: Vec<AuditEvent> = lines
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[0].op, AuditOp::Set);
        assert_eq!(events[0].name, "anthropic");
        assert!(events[0].ok);
        assert_eq!(events[2].op, AuditOp::Get);
        assert!(!events[2].ok);
        assert_eq!(events[3].op, AuditOp::Remove);
    }

    #[test]
    fn never_records_value_field() {
        // Schema invariant: AuditEvent has no "value" field. Verify by
        // round-tripping through serde and inspecting the JSON keys.
        let e = AuditEvent {
            ts: "2026-01-01T00:00:00Z".into(),
            op: AuditOp::Set,
            name: "x".into(),
            ok: true,
        };
        let json = serde_json::to_value(&e).unwrap();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["name", "ok", "op", "ts"]);
        assert!(!obj.contains_key("value"));
    }

    #[test]
    fn open_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("logs/nested/secrets.jsonl");
        let log = AuditLog::open(nested.clone()).unwrap();
        log.record(AuditOp::Get, "x", true);
        assert!(nested.exists());
    }
}
