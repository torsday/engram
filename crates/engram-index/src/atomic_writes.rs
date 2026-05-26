//! Atomic markdown + sidecar + SQLite writes via a durable
//! `atomic_write_sessions` log and POSIX atomic-rename semantics.
//!
//! Distinct from the pre-existing `write_intents` table (migration 001),
//! which is an *advisory-lock registry* for note-level conflict detection
//! (agents signal "I'm about to write this note", `expires_at` bounds the
//! lock). `atomic_write_sessions` is the WAL-style log for the *durability*
//! protocol — it tracks the lifecycle of a single two-phase write so a
//! crash between rename and SQLite commit is recoverable on restart.
//!
//! # The triple
//!
//! Every agent-driven note write touches three storage layers that must
//! land together-or-not-at-all:
//!
//! 1. **Markdown file** at `<vault>/.../<title>.md` — the canonical source.
//! 2. **Sidecar JSON** at `<vault>/.engram/sidecar/<id>.json` — rich
//!    metadata that doesn't fit cleanly in YAML frontmatter.
//! 3. **SQLite row(s)** in the metadata index — search/graph/tag state.
//!
//! [LanceDB embeddings are deliberately downstream of this triple — they
//! land asynchronously and reconcile separately, per ADR 0014.](
//!   ../docs/design/adrs/0014-lancedb-vector-storage.md)
//!
//! # The protocol
//!
//! Sessions follow a two-phase pattern keyed by an [`IntentId`] (ULID
//! generated at `begin`):
//!
//! ```text
//! 1. begin(agent_id, markdown_path, sidecar_path, expected_hash)
//!     → INSERT INTO atomic_write_sessions (..., status='begun')
//! 2. write_markdown(content)
//!     → write <markdown_path>.tmp.<intent_id>, fsync
//! 3. write_sidecar(json)
//!     → write <sidecar_path>.tmp.<intent_id>, fsync
//! 4. commit(&mut sqlite_txn)
//!     → rename(2) both .tmp files to their final names
//!     → UPDATE atomic_write_sessions SET status='committed' in caller's txn
//!     → caller commits txn; row becomes durable atomically with the metadata
//! 5. drop session (Drop is a no-op for committed sessions)
//! ```
//!
//! If the process dies between any of steps 2–4, [`recover_orphaned`] is
//! called on the next startup. It scans rows where `status='begun'` and
//! decides per-intent:
//!
//! - **Both `.tmp` files present** → the rename never happened. Replay it
//!   (idempotent: rename writes to a fresh inode, target either doesn't
//!   exist or is identical content). Mark `committed`.
//! - **One or zero `.tmp` files present** → roll back. Remove any
//!   surviving `.tmp`. Mark `rolled_back`.
//!
//! # Why this lives in `engram-index`
//!
//! The `atomic_write_sessions` log is a SQLite table and the recovery
//! scan is a SQLite query — `engram-index` already owns rusqlite. `engram-core`
//! deliberately stays SQLite-free (it's the pure shared-types crate);
//! pulling rusqlite into `engram-core` would invert the dependency
//! graph. Per ADR 0009, the markdown/sidecar writes here are
//! unstaged-file writes and never invoke `git add` / `git commit`.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{params, Connection, Transaction};
use serde_json::Value;
use thiserror::Error;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// IDs and errors
// ---------------------------------------------------------------------------

/// 26-character Crockford-base32 ULID identifying a single write session.
///
/// The id is generated at [`AtomicWriteSession::begin`] and reused as the
/// suffix on the `.tmp` filenames so recovery can pair an `.tmp` with its
/// row in `atomic_write_sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IntentId(String);

impl IntentId {
    /// Fresh ULID (monotonic clock, default rng).
    pub fn new() -> Self {
        Self(Ulid::new().to_string())
    }
    /// View as the underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for IntentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for IntentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors returned by [`AtomicWriteSession`] and [`recover_orphaned`].
#[derive(Debug, Error)]
pub enum AtomicWriteError {
    /// `rusqlite` returned an error for either the intent INSERT or the
    /// recovery scan.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Filesystem I/O failed (write, fsync, rename, or remove).
    #[error("filesystem io error at {path:?}: {source}")]
    Io {
        /// Path the operation was attempting to touch (best-effort context).
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// `commit` was called but no prior `write_markdown` or
    /// `write_sidecar` had been invoked. The session has nothing to
    /// rename.
    #[error("cannot commit: {what} was never written for intent {intent}")]
    NothingToCommit {
        /// Which piece is missing — `"markdown"` or `"sidecar"`.
        what: &'static str,
        /// Intent id for diagnostics.
        intent: IntentId,
    },

    /// `write_sidecar` was called on a session begun in markdown-only
    /// mode. The session's contract excludes the sidecar entirely;
    /// callers should either drop the sidecar write or use the
    /// paired-mode [`AtomicWriteSession::begin`] constructor.
    #[error("cannot write sidecar: intent {intent} was begun in markdown-only mode")]
    SidecarNotPermitted {
        /// Intent id for diagnostics.
        intent: IntentId,
    },
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, AtomicWriteError>;

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A single in-flight atomic write spanning a markdown file, a sidecar
/// JSON file, and (on `commit`) a SQLite row update.
///
/// Construct with [`AtomicWriteSession::begin`]; finalise with
/// [`AtomicWriteSession::commit`] or [`AtomicWriteSession::rollback`].
///
/// Per the issue's implementation note: `Drop` deliberately does **not**
/// commit or roll back. Explicit termination is required so panic paths
/// don't trigger state-changing operations. If the caller drops the
/// session without commit/rollback, the `.tmp` files and the
/// `atomic_write_sessions` row remain — [`recover_orphaned`] cleans
/// those up on the next startup. (A `tracing::warn!` flags the case so it doesn't
/// pass silently in tests / dev.)
/// Commit contract for an [`AtomicWriteSession`].
///
/// `Paired` requires both a markdown and a sidecar write before
/// `commit()` succeeds — the original behaviour from migration 003,
/// used by the curator's atomic markdown + sidecar + SQLite flow per
/// ADR 0014.
///
/// `MarkdownOnly` skips the sidecar entirely — the constructor takes
/// no sidecar path, `commit()` succeeds without a sidecar write,
/// `rollback()` doesn't try to clean a sidecar tmp file, and
/// [`recover_orphaned`] reads the session's `mode` column and applies
/// the right semantics. Used by the agent runtime (#27) for its
/// AutoLand write path, where sidecar generation is its own future
/// slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Markdown + sidecar; commit requires both.
    Paired,
    /// Markdown only; sidecar is not part of the contract.
    MarkdownOnly,
}

// `SessionMode` is purely runtime — the persisted form is the
// `mode` TEXT column written as `'paired'` / `'markdown_only'`
// inline in the INSERT statements. Keeping it that way avoids
// an indirection that an `as_sql` helper would add.

pub struct AtomicWriteSession {
    /// Owned ULID; once consumed by `commit`/`rollback` the session is
    /// invalid for further calls.
    intent_id: IntentId,
    /// Caller-provided agent name. Free-form.
    agent_id: String,
    /// Commit contract — see [`SessionMode`].
    mode: SessionMode,
    /// Final destination for the markdown body. Always set.
    target_path: PathBuf,
    /// `.tmp` destination for the markdown body. Always set.
    tmp_path: PathBuf,
    /// Final destination for the sidecar JSON. `None` in markdown-only mode.
    target_sidecar: Option<PathBuf>,
    /// `.tmp` destination for the sidecar JSON. `None` in markdown-only mode.
    tmp_sidecar: Option<PathBuf>,
    /// Tracks whether the markdown write has happened — `commit` refuses
    /// without it in either mode.
    wrote_markdown: bool,
    /// Tracks whether the sidecar write has happened. In Paired mode
    /// `commit` refuses without it; in MarkdownOnly mode it stays
    /// `false` and is ignored.
    wrote_sidecar: bool,
    /// Suppresses the drop warning when commit/rollback completes.
    finalised: bool,
}

impl AtomicWriteSession {
    /// Begin a new write session. Inserts a row into
    /// `atomic_write_sessions` with `status='begun'` and computes the
    /// `.tmp.<intent_id>` paths.
    ///
    /// `expected_diff_hash` is the caller-computed SHA-256 fingerprint of
    /// the bytes that will be written — it isn't enforced here, but is
    /// recorded so recovery / audit tools can detect a stale replay.
    pub fn begin(
        conn: &Connection,
        agent_id: impl Into<String>,
        target_path: impl Into<PathBuf>,
        target_sidecar: impl Into<PathBuf>,
        expected_diff_hash: impl Into<String>,
    ) -> Result<Self> {
        let agent_id = agent_id.into();
        let target_path = target_path.into();
        let target_sidecar = target_sidecar.into();
        let expected_diff_hash = expected_diff_hash.into();
        let intent_id = IntentId::new();

        let tmp_path = tmp_name(&target_path, &intent_id);
        let tmp_sidecar = tmp_name(&target_sidecar, &intent_id);

        let started_at = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339();

        conn.execute(
            "INSERT INTO atomic_write_sessions \
             (intent_id, agent_id, target_path, target_sidecar, expected_diff_hash, status, started_at, mode) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'begun', ?6, 'paired')",
            params![
                intent_id.as_str(),
                &agent_id,
                target_path.to_string_lossy(),
                target_sidecar.to_string_lossy(),
                &expected_diff_hash,
                &started_at,
            ],
        )?;

        Ok(Self {
            intent_id,
            agent_id,
            mode: SessionMode::Paired,
            target_path,
            tmp_path,
            target_sidecar: Some(target_sidecar),
            tmp_sidecar: Some(tmp_sidecar),
            wrote_markdown: false,
            wrote_sidecar: false,
            finalised: false,
        })
    }

    /// Begin a markdown-only session. Same as [`begin`] but no sidecar
    /// path is required: `commit()` succeeds with just `write_markdown`,
    /// `rollback()` doesn't try to clean a sidecar tmp, and
    /// [`recover_orphaned`] reads the session's `mode` column to apply
    /// the right semantics.
    ///
    /// The `target_sidecar` column in `atomic_write_sessions` is set to
    /// an empty string for markdown-only sessions — recovery checks
    /// `mode` first and ignores the sidecar column in that case.
    ///
    /// Used by the agent runtime (#27) for AutoLand writes where sidecar
    /// generation is its own future slice.
    pub fn begin_markdown_only(
        conn: &Connection,
        agent_id: impl Into<String>,
        target_path: impl Into<PathBuf>,
        expected_diff_hash: impl Into<String>,
    ) -> Result<Self> {
        let agent_id = agent_id.into();
        let target_path = target_path.into();
        let expected_diff_hash = expected_diff_hash.into();
        let intent_id = IntentId::new();

        let tmp_path = tmp_name(&target_path, &intent_id);

        let started_at = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339();

        conn.execute(
            "INSERT INTO atomic_write_sessions \
             (intent_id, agent_id, target_path, target_sidecar, expected_diff_hash, status, started_at, mode) \
             VALUES (?1, ?2, ?3, '', ?4, 'begun', ?5, 'markdown_only')",
            params![
                intent_id.as_str(),
                &agent_id,
                target_path.to_string_lossy(),
                &expected_diff_hash,
                &started_at,
            ],
        )?;

        Ok(Self {
            intent_id,
            agent_id,
            mode: SessionMode::MarkdownOnly,
            target_path,
            tmp_path,
            target_sidecar: None,
            tmp_sidecar: None,
            wrote_markdown: false,
            wrote_sidecar: false,
            finalised: false,
        })
    }

    /// The commit contract this session was begun with.
    pub fn mode(&self) -> SessionMode {
        self.mode
    }

    /// The ULID assigned to this session.
    pub fn intent_id(&self) -> &IntentId {
        &self.intent_id
    }

    /// Write the markdown body to the temporary file, fsync, and mark it
    /// as having been written.
    pub fn write_markdown(&mut self, content: &str) -> Result<()> {
        write_tmp_fsync(&self.tmp_path, content.as_bytes())?;
        self.wrote_markdown = true;
        Ok(())
    }

    /// Serialize and write the sidecar JSON to its temporary file, fsync,
    /// and mark it as having been written.
    ///
    /// Errors with [`AtomicWriteError::SidecarNotPermitted`] when the
    /// session was begun in markdown-only mode.
    pub fn write_sidecar(&mut self, json: &Value) -> Result<()> {
        let tmp_sidecar =
            self.tmp_sidecar
                .as_ref()
                .ok_or_else(|| AtomicWriteError::SidecarNotPermitted {
                    intent: self.intent_id.clone(),
                })?;
        // Pretty-print so a human reading the file on disk sees the same
        // shape Obsidian-side tools would dump; the exact serialization
        // shouldn't affect correctness.
        let bytes = serde_json::to_vec_pretty(json).map_err(|e| AtomicWriteError::Io {
            path: tmp_sidecar.clone(),
            source: std::io::Error::other(e.to_string()),
        })?;
        write_tmp_fsync(tmp_sidecar, &bytes)?;
        self.wrote_sidecar = true;
        Ok(())
    }

    /// Atomically commit the session inside the caller-provided
    /// SQLite transaction.
    ///
    /// The sequence is:
    ///
    /// 1. Verify both `.tmp` files were written. If not, error and leave
    ///    the intent in `begun` state for recovery to roll back.
    /// 2. POSIX `rename(2)` both `.tmp` files to their final paths.
    ///    Atomic on the same filesystem (which the test in
    ///    `tests/recovery.rs` verifies the typical case).
    /// 3. UPDATE the intent row to `status='committed'` *inside the
    ///    caller's transaction*. The caller's `txn.commit()` flushes this
    ///    alongside whatever note-metadata changes it made.
    pub fn commit(mut self, txn: &mut Transaction<'_>) -> Result<()> {
        if !self.wrote_markdown {
            return Err(AtomicWriteError::NothingToCommit {
                what: "markdown",
                intent: self.intent_id.clone(),
            });
        }
        if matches!(self.mode, SessionMode::Paired) && !self.wrote_sidecar {
            return Err(AtomicWriteError::NothingToCommit {
                what: "sidecar",
                intent: self.intent_id.clone(),
            });
        }

        atomic_rename(&self.tmp_path, &self.target_path)?;
        // Sidecar rename only when paired; markdown-only sessions
        // never wrote a sidecar tmp.
        if let (Some(tmp_sc), Some(target_sc)) = (&self.tmp_sidecar, &self.target_sidecar) {
            atomic_rename(tmp_sc, target_sc)?;
        }

        let committed_at = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339();
        txn.execute(
            "UPDATE atomic_write_sessions SET status='committed', committed_at=?2 WHERE intent_id=?1",
            params![self.intent_id.as_str(), &committed_at],
        )?;

        self.finalised = true;
        Ok(())
    }

    /// Roll back the session: remove any `.tmp` files that exist and mark
    /// the intent row `rolled_back`. Uses a fresh connection statement
    /// rather than a caller transaction because rollback is the
    /// independent error-recovery path.
    ///
    /// Idempotent: calling rollback on an already-rolled-back or never-
    /// started session is safe.
    pub fn rollback(mut self, conn: &Connection) -> Result<()> {
        // Best-effort: ignore NotFound errors on the .tmp removals — the
        // tmp may simply have never been written in the first place.
        if self.tmp_path.exists() {
            std::fs::remove_file(&self.tmp_path).map_err(|e| AtomicWriteError::Io {
                path: self.tmp_path.clone(),
                source: e,
            })?;
        }
        if let Some(tmp_sidecar) = &self.tmp_sidecar {
            if tmp_sidecar.exists() {
                std::fs::remove_file(tmp_sidecar).map_err(|e| AtomicWriteError::Io {
                    path: tmp_sidecar.clone(),
                    source: e,
                })?;
            }
        }

        conn.execute(
            "UPDATE atomic_write_sessions SET status='rolled_back' WHERE intent_id=?1",
            params![self.intent_id.as_str()],
        )?;

        self.finalised = true;
        let _agent_id = &self.agent_id; // keep field used
        Ok(())
    }
}

impl Drop for AtomicWriteSession {
    fn drop(&mut self) {
        if !self.finalised {
            tracing::warn!(
                intent = %self.intent_id,
                agent = %self.agent_id,
                "engram-index atomic_writes: session dropped without commit/rollback; \
                 startup recovery will resolve the orphan"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Recovery
// ---------------------------------------------------------------------------

/// Summary of one recovery cycle. Returned from [`recover_orphaned`] so
/// callers (typically `engram serve` startup) can log a structured
/// summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Intent ids that were replayed to `committed` because both `.tmp`
    /// files were on disk.
    pub committed: Vec<IntentId>,
    /// Intent ids that were rolled back because one or both `.tmp` files
    /// were missing.
    pub rolled_back: Vec<IntentId>,
}

impl RecoveryReport {
    /// Total intents touched (committed + rolled_back).
    pub fn total(&self) -> usize {
        self.committed.len() + self.rolled_back.len()
    }
}

/// Scan `atomic_write_sessions` for rows in `status='begun'` and resolve
/// each by looking at what `.tmp` files survive on disk.
///
/// This is the on-startup entry point. Safe to call on a clean database
/// (no `begun` rows → empty report).
pub fn recover_orphaned(conn: &Connection) -> Result<RecoveryReport> {
    let mut stmt = conn.prepare(
        "SELECT intent_id, target_path, target_sidecar, mode FROM atomic_write_sessions \
         WHERE status='begun' ORDER BY started_at ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    let mut report = RecoveryReport::default();
    let now = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339();

    for (intent_id_str, target_path_str, target_sidecar_str, mode_str) in rows {
        let intent_id = IntentId(intent_id_str);
        let target_path = PathBuf::from(&target_path_str);
        let tmp_path = tmp_name(&target_path, &intent_id);
        let is_paired = mode_str.as_str() == "paired";

        // Build optional sidecar paths only when the session was paired
        // — markdown-only sessions stored an empty target_sidecar at
        // begin() time and have no companion tmp file.
        let (target_sidecar, tmp_sidecar): (Option<PathBuf>, Option<PathBuf>) = if is_paired {
            let target_sidecar = PathBuf::from(&target_sidecar_str);
            let tmp_sidecar = tmp_name(&target_sidecar, &intent_id);
            (Some(target_sidecar), Some(tmp_sidecar))
        } else {
            (None, None)
        };

        // "Tmp(s) present" → ready to commit. For paired mode this
        // requires BOTH .tmp files; for markdown-only just the markdown.
        let ready_to_commit = if is_paired {
            tmp_path.exists() && tmp_sidecar.as_ref().is_some_and(|p| p.exists())
        } else {
            tmp_path.exists()
        };

        if ready_to_commit {
            atomic_rename(&tmp_path, &target_path)?;
            if let (Some(tmp_sc), Some(target_sc)) = (&tmp_sidecar, &target_sidecar) {
                atomic_rename(tmp_sc, target_sc)?;
            }
            conn.execute(
                "UPDATE atomic_write_sessions SET status='committed', committed_at=?2 WHERE intent_id=?1",
                params![intent_id.as_str(), &now],
            )?;
            tracing::info!(
                intent = %intent_id,
                mode = %mode_str,
                "engram-index atomic_writes: recovered orphan intent → committed"
            );
            report.committed.push(intent_id);
        } else {
            // Rollback: remove any survivor tmp(s).
            if tmp_path.exists() {
                std::fs::remove_file(&tmp_path).map_err(|e| AtomicWriteError::Io {
                    path: tmp_path.clone(),
                    source: e,
                })?;
            }
            if let Some(tmp_sc) = &tmp_sidecar {
                if tmp_sc.exists() {
                    std::fs::remove_file(tmp_sc).map_err(|e| AtomicWriteError::Io {
                        path: tmp_sc.clone(),
                        source: e,
                    })?;
                }
            }
            conn.execute(
                "UPDATE atomic_write_sessions SET status='rolled_back' WHERE intent_id=?1",
                params![intent_id.as_str()],
            )?;
            tracing::info!(
                intent = %intent_id,
                mode = %mode_str,
                "engram-index atomic_writes: recovered orphan intent → rolled_back"
            );
            report.rolled_back.push(intent_id);
        }
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Compute the `.tmp.<intent_id>` companion path for a final target.
///
/// The suffix-append approach (rather than splitting at the file
/// extension) preserves any existing dotted-name structure — e.g.
/// `note.with.dots.md.tmp.01J...` is unambiguous.
fn tmp_name(final_path: &Path, intent: &IntentId) -> PathBuf {
    let mut s = final_path.as_os_str().to_os_string();
    s.push(".tmp.");
    s.push(intent.as_str());
    PathBuf::from(s)
}

/// Open `path` for write+create+truncate, write all of `bytes`, fsync,
/// close. Errors are wrapped with the path for context.
fn write_tmp_fsync(path: &Path, bytes: &[u8]) -> Result<()> {
    // Ensure the parent directory exists; otherwise the rename(2) on
    // commit will fail with ENOENT and recovery can't tell that case
    // apart from "operator deleted the directory".
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicWriteError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let mut f: File = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|e| AtomicWriteError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    f.write_all(bytes).map_err(|e| AtomicWriteError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    f.sync_all().map_err(|e| AtomicWriteError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

/// POSIX `rename(2)` — atomic on the same filesystem. Wraps errors with
/// the source path for context.
fn atomic_rename(src: &Path, dst: &Path) -> Result<()> {
    std::fs::rename(src, dst).map_err(|e| AtomicWriteError::Io {
        path: src.to_path_buf(),
        source: e,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::Migrator;
    use serde_json::json;
    use tempfile::TempDir;

    /// Fresh in-memory connection with all migrations applied.
    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        Migrator::new(&conn)
            .apply_all()
            .expect("migrations apply cleanly");
        conn
    }

    /// Build a vault tempdir with the standard layout and return paths for
    /// a single note write.
    struct VaultPaths {
        _dir: TempDir,
        md: PathBuf,
        sidecar: PathBuf,
    }

    fn vault_paths() -> VaultPaths {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("notes").join("alpha.md");
        let sidecar = dir
            .path()
            .join(".engram")
            .join("sidecar")
            .join("alpha.json");
        VaultPaths {
            _dir: dir,
            md,
            sidecar,
        }
    }

    #[test]
    fn happy_path_commit_writes_both_final_files_and_marks_committed() {
        let mut conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "deadbeef")
            .expect("begin");
        s.write_markdown("# hello\n\nbody\n").unwrap();
        s.write_sidecar(&json!({"id": "01JX", "neighbors": []}))
            .unwrap();
        let intent = s.intent_id().clone();
        let mut txn = conn.transaction().unwrap();
        s.commit(&mut txn).unwrap();
        txn.commit().unwrap();

        assert!(p.md.exists(), "final markdown must exist");
        assert!(p.sidecar.exists(), "final sidecar must exist");
        assert_eq!(std::fs::read_to_string(&p.md).unwrap(), "# hello\n\nbody\n");

        let status: String = conn
            .query_row(
                "SELECT status FROM atomic_write_sessions WHERE intent_id=?1",
                params![intent.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "committed");
    }

    #[test]
    fn commit_without_markdown_returns_nothing_to_commit() {
        let mut conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        s.write_sidecar(&json!({})).unwrap();
        let mut txn = conn.transaction().unwrap();
        let err = s.commit(&mut txn).unwrap_err();
        assert!(matches!(
            err,
            AtomicWriteError::NothingToCommit {
                what: "markdown",
                ..
            }
        ));
    }

    #[test]
    fn commit_without_sidecar_returns_nothing_to_commit() {
        let mut conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        s.write_markdown("body").unwrap();
        let mut txn = conn.transaction().unwrap();
        let err = s.commit(&mut txn).unwrap_err();
        assert!(matches!(
            err,
            AtomicWriteError::NothingToCommit {
                what: "sidecar",
                ..
            }
        ));
    }

    #[test]
    fn rollback_removes_tmps_and_marks_rolled_back() {
        let conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        s.write_markdown("body").unwrap();
        s.write_sidecar(&json!({})).unwrap();
        let intent = s.intent_id().clone();
        let tmp_md = tmp_name(&p.md, &intent);
        let tmp_sc = tmp_name(&p.sidecar, &intent);
        assert!(tmp_md.exists());
        assert!(tmp_sc.exists());

        s.rollback(&conn).unwrap();

        assert!(!tmp_md.exists());
        assert!(!tmp_sc.exists());
        assert!(!p.md.exists());

        let status: String = conn
            .query_row(
                "SELECT status FROM atomic_write_sessions WHERE intent_id=?1",
                params![intent.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "rolled_back");
    }

    #[test]
    fn recovery_both_tmps_present_replays_rename() {
        let conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        s.write_markdown("body").unwrap();
        s.write_sidecar(&json!({})).unwrap();
        let intent = s.intent_id().clone();
        // Simulate crash: drop the session without commit/rollback. The
        // tracing::warn from Drop is expected; tests don't capture it.
        std::mem::forget(s); // forget so Drop doesn't run and emit warn under cargo test
                             // Now run recovery.
        let report = recover_orphaned(&conn).expect("recover");
        assert_eq!(report.committed, vec![intent.clone()]);
        assert!(report.rolled_back.is_empty());

        assert!(p.md.exists());
        assert!(p.sidecar.exists());
        let status: String = conn
            .query_row(
                "SELECT status FROM atomic_write_sessions WHERE intent_id=?1",
                params![intent.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "committed");
    }

    #[test]
    fn recovery_missing_sidecar_rolls_back() {
        let conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        s.write_markdown("body").unwrap();
        // Sidecar deliberately not written.
        let intent = s.intent_id().clone();
        std::mem::forget(s);

        let report = recover_orphaned(&conn).expect("recover");
        assert_eq!(report.rolled_back, vec![intent.clone()]);
        assert!(report.committed.is_empty());

        // The orphan markdown tmp should have been cleaned up.
        let tmp_md = tmp_name(&p.md, &intent);
        assert!(!tmp_md.exists());
        // Final files never came into existence.
        assert!(!p.md.exists());
        assert!(!p.sidecar.exists());

        let status: String = conn
            .query_row(
                "SELECT status FROM atomic_write_sessions WHERE intent_id=?1",
                params![intent.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "rolled_back");
    }

    #[test]
    fn recovery_no_tmps_present_rolls_back() {
        let conn = fresh_db();
        let p = vault_paths();
        let s = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        // Neither write_markdown nor write_sidecar called — nothing on
        // disk. This is the "begin then immediate crash" path.
        let intent = s.intent_id().clone();
        std::mem::forget(s);

        let report = recover_orphaned(&conn).expect("recover");
        assert_eq!(report.rolled_back, vec![intent]);
    }

    #[test]
    fn recovery_on_clean_db_returns_empty_report() {
        let conn = fresh_db();
        let report = recover_orphaned(&conn).expect("recover on clean db");
        assert!(report.committed.is_empty());
        assert!(report.rolled_back.is_empty());
        assert_eq!(report.total(), 0);
    }

    #[test]
    fn recovery_skips_committed_and_rolled_back_rows() {
        let mut conn = fresh_db();
        let p = vault_paths();

        // Land a committed intent.
        let mut s1 = AtomicWriteSession::begin(&conn, "linker", &p.md, &p.sidecar, "x").unwrap();
        s1.write_markdown("a").unwrap();
        s1.write_sidecar(&json!({})).unwrap();
        let mut txn = conn.transaction().unwrap();
        s1.commit(&mut txn).unwrap();
        txn.commit().unwrap();

        // Land a rolled-back intent.
        let p2 = vault_paths();
        let s2 = AtomicWriteSession::begin(&conn, "linker", &p2.md, &p2.sidecar, "x").unwrap();
        s2.rollback(&conn).unwrap();

        // Recovery should touch nothing.
        let report = recover_orphaned(&conn).expect("recover");
        assert_eq!(report.total(), 0);
    }

    #[test]
    fn tmp_name_appends_intent_suffix() {
        let id = IntentId("01JXABC".to_string());
        let p = tmp_name(Path::new("/v/notes/x.md"), &id);
        assert_eq!(p, PathBuf::from("/v/notes/x.md.tmp.01JXABC"));
    }

    #[test]
    fn intent_id_round_trip() {
        let a = IntentId::new();
        let b = IntentId::new();
        assert_ne!(a, b, "two fresh ULIDs must differ");
        assert_eq!(a.as_str().len(), 26);
    }

    // ── markdown-only mode ───────────────────────────────────────────

    #[test]
    fn markdown_only_commit_succeeds_without_sidecar_write() {
        let mut conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin_markdown_only(&conn, "linker", &p.md, "deadbeef")
            .expect("begin_markdown_only");
        assert_eq!(s.mode(), SessionMode::MarkdownOnly);
        s.write_markdown("# only the body\n").unwrap();
        let intent = s.intent_id().clone();
        let mut txn = conn.transaction().unwrap();
        s.commit(&mut txn).unwrap();
        txn.commit().unwrap();

        assert!(p.md.exists(), "final markdown must exist");
        assert!(
            !p.sidecar.exists(),
            "sidecar must NOT exist in markdown-only mode"
        );

        let (status, mode): (String, String) = conn
            .query_row(
                "SELECT status, mode FROM atomic_write_sessions WHERE intent_id=?1",
                params![intent.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "committed");
        assert_eq!(mode, "markdown_only");
    }

    #[test]
    fn markdown_only_commit_without_markdown_still_errors() {
        // The markdown-only relaxation removes the sidecar requirement
        // but NOT the markdown requirement — a session that never
        // wrote markdown is still NothingToCommit.
        let mut conn = fresh_db();
        let p = vault_paths();
        let s = AtomicWriteSession::begin_markdown_only(&conn, "linker", &p.md, "x").unwrap();
        let mut txn = conn.transaction().unwrap();
        let err = s.commit(&mut txn).unwrap_err();
        assert!(matches!(
            err,
            AtomicWriteError::NothingToCommit {
                what: "markdown",
                ..
            }
        ));
    }

    #[test]
    fn markdown_only_write_sidecar_errors() {
        let conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin_markdown_only(&conn, "linker", &p.md, "x").unwrap();
        let err = s.write_sidecar(&json!({})).unwrap_err();
        assert!(matches!(err, AtomicWriteError::SidecarNotPermitted { .. }));
    }

    #[test]
    fn markdown_only_rollback_does_not_touch_sidecar() {
        let conn = fresh_db();
        let p = vault_paths();
        let mut s = AtomicWriteSession::begin_markdown_only(&conn, "linker", &p.md, "x").unwrap();
        s.write_markdown("body\n").unwrap();
        let intent = s.intent_id().clone();
        // Pre-existing sidecar at the expected path should be left
        // alone — markdown-only never owns it.
        std::fs::create_dir_all(p.sidecar.parent().unwrap()).unwrap();
        std::fs::write(&p.sidecar, "pre-existing").unwrap();

        s.rollback(&conn).unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM atomic_write_sessions WHERE intent_id=?1",
                params![intent.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "rolled_back");
        // Markdown tmp gone, pre-existing sidecar untouched.
        let tmp_path = tmp_name(&p.md, &intent);
        assert!(!tmp_path.exists());
        assert_eq!(std::fs::read_to_string(&p.sidecar).unwrap(), "pre-existing");
    }

    #[test]
    fn recovery_replays_markdown_only_orphan() {
        // Simulate a process death after write_markdown but before
        // commit on a markdown-only session: the row is `begun`, the
        // .tmp file is on disk, recover_orphaned should rename and
        // mark committed.
        let conn = fresh_db();
        let p = vault_paths();
        {
            let mut s =
                AtomicWriteSession::begin_markdown_only(&conn, "linker", &p.md, "x").unwrap();
            s.write_markdown("orphaned body\n").unwrap();
            s.finalised = true; // suppress the drop warning; we're simulating death
        }
        let report = recover_orphaned(&conn).unwrap();
        assert_eq!(report.committed.len(), 1);
        assert_eq!(report.rolled_back.len(), 0);
        assert!(p.md.exists());
        assert_eq!(std::fs::read_to_string(&p.md).unwrap(), "orphaned body\n");
        assert!(
            !p.sidecar.exists(),
            "recovery must not create a sidecar for markdown-only sessions"
        );
    }

    #[test]
    fn recovery_rolls_back_markdown_only_orphan_with_missing_tmp() {
        let conn = fresh_db();
        let p = vault_paths();
        {
            // Begin but never write — .tmp missing.
            let s = AtomicWriteSession::begin_markdown_only(&conn, "linker", &p.md, "x").unwrap();
            std::mem::forget(s); // simulate death between begin and write_markdown
        }
        let report = recover_orphaned(&conn).unwrap();
        assert_eq!(report.committed.len(), 0);
        assert_eq!(report.rolled_back.len(), 1);
    }

    #[test]
    fn mixed_mode_recovery_classifies_each_correctly() {
        // One paired-mode orphan (both .tmps present) → committed.
        // One markdown-only orphan (just .md.tmp present) → committed.
        // Recovery walks both correctly using the `mode` column.
        let conn = fresh_db();
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        // Paired session.
        let md1 = dir1.path().join("a.md");
        let sc1 = dir1.path().join(".engram/sidecar/a.json");
        {
            let mut s = AtomicWriteSession::begin(&conn, "ag", &md1, &sc1, "h1").unwrap();
            s.write_markdown("paired body\n").unwrap();
            s.write_sidecar(&json!({"id": "1"})).unwrap();
            s.finalised = true;
        }

        // Markdown-only session.
        let md2 = dir2.path().join("b.md");
        {
            let mut s = AtomicWriteSession::begin_markdown_only(&conn, "ag", &md2, "h2").unwrap();
            s.write_markdown("md-only body\n").unwrap();
            s.finalised = true;
        }

        let report = recover_orphaned(&conn).unwrap();
        assert_eq!(report.committed.len(), 2);
        assert!(md1.exists());
        assert!(sc1.exists());
        assert!(md2.exists());
    }
}
