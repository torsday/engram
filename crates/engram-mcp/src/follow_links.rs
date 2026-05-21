//! `follow_links` MCP tool — notes the given note wikilinks to (outgoing).
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "note_id":      "<ULID>",   // one of these is required
//!   "slug":         "<slug>",
//!   "path":         "<rel path>",
//!   "include_dead": true        // optional, default true — include unresolved links
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "links": [
//!     {
//!       "target_id":   "<ULID or null>",
//!       "target_slug": "<slug or null>",
//!       "anchor":      "<wikilink text>",
//!       "line_number": 5,
//!       "resolved":    true
//!     }
//!   ]
//! }
//! ```
//!
//! ## Notes
//!
//! Forward-link resolution requires the wikilink parser and link-graph index
//! (`engram-core::wikilink`, `engram-index::link_graph`). Until those are
//! built the tool returns an empty `links` list and a `links_note` explaining
//! why. This is intentional and correct — the tool surface is stable; the
//! underlying data populates once the index runs.
//!
//! ## Error codes
//!
//! | code          | meaning                              |
//! |---------------|--------------------------------------|
//! | `bad_input`   | None of id/slug/path provided        |
//! | `not_found`   | Note not found in the vault          |

use std::path::Path;

use serde::{Deserialize, Serialize};

use engram_core::vault::{read_note, NoteKey, VaultError};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `follow_links` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct FollowLinksInput {
    /// Look up source by ULID.
    pub note_id: Option<String>,
    /// Look up source by title slug.
    pub slug: Option<String>,
    /// Look up source by relative path.
    pub path: Option<String>,
    /// Include unresolved (dead) links in the result (default true).
    #[serde(default = "default_include_dead")]
    pub include_dead: bool,
}

fn default_include_dead() -> bool {
    true
}

/// One forward-link entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LinkEntry {
    /// ULID of the target note, or `null` when unresolved.
    pub target_id: Option<String>,
    /// Slug of the target note, or `null` when unresolved.
    pub target_slug: Option<String>,
    /// The wikilink anchor text as written in the source note.
    pub anchor: String,
    /// 1-based line number of the wikilink in the source note.
    pub line_number: usize,
    /// Whether the target note was found in the vault.
    pub resolved: bool,
}

/// Successful output for the `follow_links` tool.
#[derive(Debug, Clone, Serialize)]
pub struct FollowLinksOutput {
    /// Forward links found in the note. Empty when wikilink parsing is not yet wired.
    pub links: Vec<LinkEntry>,
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

/// Execute the `follow_links` tool.
pub fn handle(vault_root: &Path, input: FollowLinksInput) -> Result<FollowLinksOutput, ToolError> {
    let key = resolve_key(&input.note_id, &input.slug, &input.path)?;

    // Verify the note exists (surfaces not_found early).
    read_note(vault_root, key, false).map_err(map_vault_error)?;

    // Wikilink parser + link-graph not yet wired — return empty links.
    // Will be populated once engram-core::wikilink and engram-index::link_graph land.
    Ok(FollowLinksOutput { links: vec![] })
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
            "---\nid: AAAA1234567890123456789012\ntitle: My Note\ntype: evergreen\n---\n\nSee [[Other Note]].",
        )
        .unwrap();
        dir
    }

    #[test]
    fn returns_empty_links_for_existing_note() {
        let vault = fixture_vault();
        let out = handle(
            vault.path(),
            FollowLinksInput {
                slug: Some("my-note".into()),
                note_id: None,
                path: None,
                include_dead: true,
            },
        )
        .unwrap();
        assert!(out.links.is_empty());
    }

    #[test]
    fn not_found_for_missing_note() {
        let vault = fixture_vault();
        let err = handle(
            vault.path(),
            FollowLinksInput {
                slug: Some("nope".into()),
                note_id: None,
                path: None,
                include_dead: true,
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
            FollowLinksInput {
                note_id: None,
                slug: None,
                path: None,
                include_dead: true,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn output_schema_has_links_field() {
        let vault = fixture_vault();
        let out = handle(
            vault.path(),
            FollowLinksInput {
                slug: Some("my-note".into()),
                note_id: None,
                path: None,
                include_dead: true,
            },
        )
        .unwrap();
        let json = serde_json::to_value(&out).unwrap();
        assert!(json.get("links").is_some());
    }

    #[test]
    fn ambiguous_key_returns_bad_input() {
        let vault = fixture_vault();
        let err = handle(
            vault.path(),
            FollowLinksInput {
                slug: Some("my-note".into()),
                note_id: Some("AAAA1234567890123456789012".into()),
                path: None,
                include_dead: true,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }
}
