//! `follow_backlinks` MCP tool — notes that wikilink to the given note (incoming).
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "note_id": "<ULID>",    // one of these is required
//!   "slug":    "<slug>",
//!   "path":    "<rel path>",
//!   "depth":   1             // optional, default 1, max 3
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "backlinks": [
//!     {
//!       "note_id": "<ULID>",
//!       "title":   "<string>",
//!       "path":    "<absolute path>",
//!       "anchors": ["<wikilink text>"],
//!       "depth":   1
//!     }
//!   ]
//! }
//! ```
//!
//! ## Notes
//!
//! Backlinks require a live link-graph index (`engram-index::link_graph`).
//! Until that index is built the tool returns an empty `backlinks` list —
//! this is correct (no links indexed = no known backlinks) and will
//! auto-populate once the index is running.
//!
//! ## Error codes
//!
//! | code          | meaning                              |
//! |---------------|--------------------------------------|
//! | `bad_input`   | None of id/slug/path provided, or depth out of range |
//! | `not_found`   | Note not found in the vault          |

use std::path::Path;

use serde::{Deserialize, Serialize};

use engram_core::vault::{read_note, NoteKey, VaultError};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `follow_backlinks` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct FollowBacklinksInput {
    /// Look up target by ULID.
    pub note_id: Option<String>,
    /// Look up target by title slug.
    pub slug: Option<String>,
    /// Look up target by relative path.
    pub path: Option<String>,
    /// Traversal depth (default 1, max 3).
    #[serde(default = "default_depth")]
    pub depth: u32,
}

fn default_depth() -> u32 {
    1
}

/// One resolved backlink entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BacklinkEntry {
    /// ULID of the linking note.
    pub note_id: String,
    /// Title of the linking note.
    pub title: String,
    /// Absolute path to the linking note.
    pub path: String,
    /// Wikilink anchor texts found in the linking note.
    pub anchors: Vec<String>,
    /// Traversal depth at which this note was found.
    pub depth: u32,
}

/// Successful output for the `follow_backlinks` tool.
#[derive(Debug, Clone, Serialize)]
pub struct FollowBacklinksOutput {
    /// Resolved backlinks. Empty when the link graph is not yet built.
    pub backlinks: Vec<BacklinkEntry>,
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

/// Execute the `follow_backlinks` tool.
pub fn handle(
    vault_root: &Path,
    input: FollowBacklinksInput,
) -> Result<FollowBacklinksOutput, ToolError> {
    if input.depth == 0 || input.depth > 3 {
        return Err(ToolError {
            code: "bad_input".into(),
            message: "depth must be between 1 and 3".into(),
        });
    }

    let key = resolve_key(&input.note_id, &input.slug, &input.path)?;

    // Verify the note exists (surfaces not_found early).
    read_note(vault_root, key, false).map_err(map_vault_error)?;

    // Link graph is not yet built — return empty backlinks.
    // This will be populated once engram-index::link_graph is wired in.
    Ok(FollowBacklinksOutput { backlinks: vec![] })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_key<'a>(
    note_id: &'a Option<String>,
    slug: &'a Option<String>,
    path: &'a Option<String>,
) -> Result<NoteKey<'a>, ToolError> {
    match (note_id, slug, path) {
        (Some(id), None, None) => Ok(NoteKey::Id(id)),
        (None, Some(s), None) => Ok(NoteKey::Slug(s)),
        (None, None, Some(p)) => Ok(NoteKey::Path(p)),
        (None, None, None) => Err(ToolError {
            code: "bad_input".into(),
            message: "one of 'note_id', 'slug', or 'path' must be provided".into(),
        }),
        _ => Err(ToolError {
            code: "bad_input".into(),
            message: "only one of 'note_id', 'slug', or 'path' may be provided".into(),
        }),
    }
}

fn map_vault_error(e: VaultError) -> ToolError {
    match e {
        VaultError::NotFound(k) => ToolError {
            code: "not_found".into(),
            message: format!("no note found for {k}"),
        },
        VaultError::NotADirectory(p) => ToolError {
            code: "vault_not_configured".into(),
            message: format!("vault root is not a directory: {}", p.display()),
        },
        e => ToolError {
            code: "io_error".into(),
            message: e.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture_vault() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("my-note.md"),
            "---\nid: AAAA1234567890123456789012\ntitle: My Note\ntype: evergreen\n---\n\nBody.",
        )
        .unwrap();
        dir
    }

    #[test]
    fn returns_empty_backlinks_for_existing_note() {
        let vault = fixture_vault();
        let out = handle(
            vault.path(),
            FollowBacklinksInput {
                slug: Some("my-note".into()),
                note_id: None,
                path: None,
                depth: 1,
            },
        )
        .unwrap();
        assert!(out.backlinks.is_empty());
    }

    #[test]
    fn not_found_for_missing_note() {
        let vault = fixture_vault();
        let err = handle(
            vault.path(),
            FollowBacklinksInput {
                slug: Some("nope".into()),
                note_id: None,
                path: None,
                depth: 1,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn no_key_returns_bad_input() {
        let vault = fixture_vault();
        let err = handle(
            vault.path(),
            FollowBacklinksInput {
                note_id: None,
                slug: None,
                path: None,
                depth: 1,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn depth_out_of_range_returns_bad_input() {
        let vault = fixture_vault();
        let err = handle(
            vault.path(),
            FollowBacklinksInput {
                slug: Some("my-note".into()),
                note_id: None,
                path: None,
                depth: 5,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn output_schema_has_backlinks_field() {
        let vault = fixture_vault();
        let out = handle(
            vault.path(),
            FollowBacklinksInput {
                slug: Some("my-note".into()),
                note_id: None,
                path: None,
                depth: 1,
            },
        )
        .unwrap();
        let json = serde_json::to_value(&out).unwrap();
        assert!(json.get("backlinks").is_some());
    }
}
