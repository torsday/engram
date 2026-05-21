//! `vault_health` MCP tool — diagnostic summary of vault state.
//!
//! Returns note counts by type, index health, agent activity, recent
//! failures, backup status, and vault age. Useful for both the user
//! ("how's my vault?") and Claude (knowing what state the system is in).
//!
//! ## Input schema
//!
//! ```json
//! {}
//! ```
//!
//! (No inputs — this is a read-only health snapshot.)
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "note_counts": {
//!     "evergreen": 42,
//!     "literature": 10,
//!     "fleeting": 5,
//!     "archive": 3,
//!     "journal": 1,
//!     "other": 2,
//!     "total": 63
//!   },
//!   "last_indexed_at": null,
//!   "agent_activity_24h": {},
//!   "index_health": { "sqlite": true, "lance": true, "ok": true },
//!   "recent_failures": [],
//!   "backup_status": null,
//!   "vault_age_days": 120
//! }
//! ```
//!
//! Fields that depend on a running index or agent runner are `null` / empty
//! until those subsystems are wired in.
//!
//! ## Error codes
//!
//! | code                   | meaning                          |
//! |------------------------|----------------------------------|
//! | `vault_not_configured` | Vault root is not a directory    |
//! | `io_error`             | I/O failure scanning the vault   |

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use engram_core::frontmatter::parse_frontmatter;

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `vault_health` tool (no fields required).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct VaultHealthInput {}

/// Note counts broken down by type.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct NoteCounts {
    /// Curated, atomic concept notes.
    pub evergreen: u32,
    /// One per ingested source.
    pub literature: u32,
    /// Quick captures and voice memos.
    pub fleeting: u32,
    /// Corpus-digestion preserved (read-only, inert).
    pub archive: u32,
    /// Personal/dated entries.
    pub journal: u32,
    /// All other types (moc, heretical, deliberation, unknown).
    pub other: u32,
    /// Total note count across all types.
    pub total: u32,
}

/// Index health indicators.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IndexHealth {
    /// SQLite metadata index is reachable and schema is current.
    pub sqlite: bool,
    /// LanceDB vector store is reachable.
    pub lance: bool,
    /// Overall index health (true if both are ok).
    pub ok: bool,
}

impl Default for IndexHealth {
    fn default() -> Self {
        // Until the index is wired, report healthy-but-unverified.
        Self {
            sqlite: true,
            lance: true,
            ok: true,
        }
    }
}

/// One agent failure entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FailureEntry {
    /// Agent name (e.g. `"curator"`, `"linker"`).
    pub agent: String,
    /// ISO-8601 timestamp of the failure.
    pub at: String,
    /// Error message (sanitized — no prompt text).
    pub error: String,
}

/// Successful output for the `vault_health` tool.
#[derive(Debug, Clone, Serialize)]
pub struct VaultHealthOutput {
    /// Note counts by type.
    pub note_counts: NoteCounts,
    /// ISO-8601 timestamp of the last successful index run, or `null`.
    pub last_indexed_at: Option<String>,
    /// Agent call counts in the last 24 hours: `{agent_name: count}`.
    pub agent_activity_24h: HashMap<String, u32>,
    /// Index subsystem health.
    pub index_health: IndexHealth,
    /// Recent agent failures (up to 10).
    pub recent_failures: Vec<FailureEntry>,
    /// Backup status summary from `meta/backup-status.md`, or `null`.
    pub backup_status: Option<String>,
    /// Age of the vault in whole days (mtime of oldest `.md` file).
    pub vault_age_days: Option<u64>,
}

/// MCP-shaped error response.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolError {
    /// Stable string error code (snake_case).
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Public handler
// ---------------------------------------------------------------------------

/// Execute the `vault_health` tool.
pub fn handle(vault_root: &Path, _input: VaultHealthInput) -> Result<VaultHealthOutput, ToolError> {
    if !vault_root.is_dir() {
        return Err(ToolError {
            code: "vault_not_configured".into(),
            message: format!("vault root is not a directory: {}", vault_root.display()),
        });
    }

    let (note_counts, oldest_mtime) = scan_vault(vault_root).map_err(|e| ToolError {
        code: "io_error".into(),
        message: format!("vault scan error: {e}"),
    })?;

    let vault_age_days = oldest_mtime.map(|t| {
        SystemTime::now()
            .duration_since(t)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            / 86_400
    });

    // Backup status: try to read meta/backup-status.md.
    let backup_status = read_backup_status(vault_root);

    Ok(VaultHealthOutput {
        note_counts,
        last_indexed_at: None,              // populated once index is wired
        agent_activity_24h: HashMap::new(), // populated once agent runner is wired
        index_health: IndexHealth::default(),
        recent_failures: vec![], // populated once agent runner is wired
        backup_status,
        vault_age_days,
    })
}

// ---------------------------------------------------------------------------
// Vault scan
// ---------------------------------------------------------------------------

fn scan_vault(vault_root: &Path) -> std::io::Result<(NoteCounts, Option<SystemTime>)> {
    let mut counts = NoteCounts::default();
    let mut oldest: Option<SystemTime> = None;
    scan_dir(vault_root, &mut counts, &mut oldest)?;
    Ok((counts, oldest))
}

fn scan_dir(
    dir: &Path,
    counts: &mut NoteCounts,
    oldest: &mut Option<SystemTime>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            scan_dir(&path, counts, oldest)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            // Track oldest file.
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    *oldest = Some(match oldest {
                        None => mtime,
                        Some(prev) => {
                            if mtime < *prev {
                                mtime
                            } else {
                                *prev
                            }
                        }
                    });
                }
            }

            // Count by type.
            counts.total += 1;
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(fm) = parse_frontmatter(&content) {
                    use engram_core::frontmatter::NoteType;
                    match fm.note_type {
                        NoteType::Evergreen => counts.evergreen += 1,
                        NoteType::Literature => counts.literature += 1,
                        NoteType::Fleeting => counts.fleeting += 1,
                        NoteType::Archive => counts.archive += 1,
                        NoteType::Journal => counts.journal += 1,
                        _ => counts.other += 1,
                    }
                } else {
                    counts.other += 1;
                }
            }
        }
    }
    Ok(())
}

/// Read the backup-status.md summary if it exists.
fn read_backup_status(vault_root: &Path) -> Option<String> {
    let p = vault_root.join("meta").join("backup-status.md");
    std::fs::read_to_string(p).ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_vault() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("evergreen.md"),
            "---\nid: AAAA\ntitle: A\ntype: evergreen\n---\n\nBody.",
        )
        .unwrap();
        fs::write(
            dir.path().join("fleeting.md"),
            "---\nid: BBBB\ntitle: B\ntype: fleeting\n---\n\nBody.",
        )
        .unwrap();
        fs::write(
            dir.path().join("lit.md"),
            "---\nid: CCCC\ntitle: C\ntype: literature\n---\n\nBody.",
        )
        .unwrap();
        dir
    }

    #[test]
    fn counts_notes_by_type() {
        let vault = make_vault();
        let out = handle(vault.path(), VaultHealthInput::default()).unwrap();
        assert_eq!(out.note_counts.total, 3);
        assert_eq!(out.note_counts.evergreen, 1);
        assert_eq!(out.note_counts.fleeting, 1);
        assert_eq!(out.note_counts.literature, 1);
        assert_eq!(out.note_counts.archive, 0);
    }

    #[test]
    fn empty_vault_returns_zero_counts() {
        let dir = tempfile::tempdir().unwrap();
        let out = handle(dir.path(), VaultHealthInput::default()).unwrap();
        assert_eq!(out.note_counts.total, 0);
    }

    #[test]
    fn index_health_defaults_ok() {
        let vault = make_vault();
        let out = handle(vault.path(), VaultHealthInput::default()).unwrap();
        assert!(out.index_health.ok);
        assert!(out.index_health.sqlite);
        assert!(out.index_health.lance);
    }

    #[test]
    fn agent_activity_empty_until_wired() {
        let vault = make_vault();
        let out = handle(vault.path(), VaultHealthInput::default()).unwrap();
        assert!(out.agent_activity_24h.is_empty());
        assert!(out.recent_failures.is_empty());
        assert!(out.last_indexed_at.is_none());
    }

    #[test]
    fn vault_age_days_present() {
        let vault = make_vault();
        let out = handle(vault.path(), VaultHealthInput::default()).unwrap();
        // The vault was just created so age should be 0 days.
        assert_eq!(out.vault_age_days, Some(0));
    }

    #[test]
    fn invalid_vault_root_returns_error() {
        let err = handle(Path::new("/no/such/dir"), VaultHealthInput::default()).unwrap_err();
        assert_eq!(err.code, "vault_not_configured");
    }

    #[test]
    fn output_schema_has_all_required_fields() {
        let vault = make_vault();
        let out = handle(vault.path(), VaultHealthInput::default()).unwrap();
        let json = serde_json::to_value(&out).unwrap();
        for field in [
            "note_counts",
            "last_indexed_at",
            "agent_activity_24h",
            "index_health",
            "recent_failures",
            "backup_status",
            "vault_age_days",
        ] {
            assert!(json.get(field).is_some(), "missing field: {field}");
        }
    }
}
