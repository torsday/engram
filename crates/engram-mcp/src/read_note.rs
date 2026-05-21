//! `read_note` MCP tool — fetch a note by id, slug, or path.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "id":              "<ULID string>",  // optional
//!   "slug":            "<title-slug>",   // optional
//!   "path":            "<rel path>",     // optional
//!   "include_sidecar": false             // optional, default false
//! }
//! ```
//!
//! Exactly one of `id`, `slug`, or `path` must be present.
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "id":          "<ULID>",
//!   "title":       "<string>",
//!   "path":        "<absolute path>",
//!   "body":        "<markdown body>",
//!   "frontmatter": { /* full parsed frontmatter */ },
//!   "sidecar":     { /* sidecar JSON or null */ },
//!   "backlinks":   [{ "from_note_id": "<ULID>", "anchor": "<string>" }]
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                  | meaning                              |
//! |-----------------------|--------------------------------------|
//! | `not_found`           | No note matched the key              |
//! | `bad_input`           | Missing or ambiguous key             |
//! | `vault_not_configured`| Vault root is not a directory        |
//! | `io_error`            | I/O failure reading the note file    |
//! | `parse_error`         | Frontmatter could not be parsed      |

use serde::{Deserialize, Serialize};

use engram_core::vault::{read_note as core_read_note, Backlink, NoteKey, NoteRecord, VaultError};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `read_note` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadNoteInput {
    /// Look up by ULID.
    pub id: Option<String>,
    /// Look up by title slug.
    pub slug: Option<String>,
    /// Look up by path relative to the vault root.
    pub path: Option<String>,
    /// Whether to include the sidecar JSON in the response.
    #[serde(default)]
    pub include_sidecar: bool,
}

/// Serializable backlink entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BacklinkDto {
    /// ULID of the linking note.
    pub from_note_id: String,
    /// Anchor text.
    pub anchor: String,
}

impl From<Backlink> for BacklinkDto {
    fn from(b: Backlink) -> Self {
        Self {
            from_note_id: b.from_note_id,
            anchor: b.anchor,
        }
    }
}

/// Successful output for the `read_note` tool.
#[derive(Debug, Clone, Serialize)]
pub struct ReadNoteOutput {
    /// ULID from frontmatter.
    pub id: String,
    /// Title from frontmatter.
    pub title: String,
    /// Absolute path to the `.md` file.
    pub path: String,
    /// Note body (after the closing frontmatter delimiter).
    pub body: String,
    /// Full frontmatter as a JSON value.
    pub frontmatter: serde_json::Value,
    /// Sidecar JSON if requested; null otherwise.
    pub sidecar: Option<serde_json::Value>,
    /// Backlinks (empty until the link graph is built).
    pub backlinks: Vec<BacklinkDto>,
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

/// Execute the `read_note` tool.
///
/// Returns `Ok(ReadNoteOutput)` on success, or a structured [`ToolError`] that
/// the MCP server wraps in an MCP error response.
pub fn handle(
    vault_root: &std::path::Path,
    input: ReadNoteInput,
) -> Result<ReadNoteOutput, ToolError> {
    let key = resolve_key(&input)?;

    let record = core_read_note(vault_root, key, input.include_sidecar).map_err(map_vault_error)?;

    to_output(record)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn resolve_key(input: &ReadNoteInput) -> Result<NoteKey<'_>, ToolError> {
    match (&input.id, &input.slug, &input.path) {
        (Some(id), None, None) => Ok(NoteKey::Id(id)),
        (None, Some(slug), None) => Ok(NoteKey::Slug(slug)),
        (None, None, Some(path)) => Ok(NoteKey::Path(path)),
        (None, None, None) => Err(ToolError {
            code: "bad_input".into(),
            message: "one of 'id', 'slug', or 'path' must be provided".into(),
        }),
        _ => Err(ToolError {
            code: "bad_input".into(),
            message: "only one of 'id', 'slug', or 'path' may be provided".into(),
        }),
    }
}

fn map_vault_error(e: VaultError) -> ToolError {
    match e {
        VaultError::NotFound(key) => ToolError {
            code: "not_found".into(),
            message: format!("no note found for {key}"),
        },
        VaultError::NotADirectory(p) => ToolError {
            code: "vault_not_configured".into(),
            message: format!("vault root is not a directory: {}", p.display()),
        },
        VaultError::Io { path, source } => ToolError {
            code: "io_error".into(),
            message: format!("I/O error reading {}: {source}", path.display()),
        },
        VaultError::Frontmatter { path, source } => ToolError {
            code: "parse_error".into(),
            message: format!("frontmatter error in {}: {source}", path.display()),
        },
        VaultError::Sidecar { source } => ToolError {
            code: "io_error".into(),
            message: format!("sidecar error: {source}"),
        },
    }
}

fn to_output(record: NoteRecord) -> Result<ReadNoteOutput, ToolError> {
    let frontmatter_val = serde_json::to_value(&record.frontmatter).map_err(|e| ToolError {
        code: "parse_error".into(),
        message: format!("could not serialize frontmatter: {e}"),
    })?;

    let sidecar_val = record
        .sidecar
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| ToolError {
            code: "parse_error".into(),
            message: format!("could not serialize sidecar: {e}"),
        })?;

    Ok(ReadNoteOutput {
        id: record.id,
        title: record.title,
        path: record.path.to_string_lossy().into_owned(),
        body: record.body,
        frontmatter: frontmatter_val,
        sidecar: sidecar_val,
        backlinks: record
            .backlinks
            .into_iter()
            .map(BacklinkDto::from)
            .collect(),
    })
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
        let note = "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: Hello World\ntype: evergreen\n---\n\nThis is the body.";
        fs::write(dir.path().join("hello-world.md"), note).unwrap();
        dir
    }

    #[test]
    fn read_by_slug_returns_expected_shape() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            slug: Some("hello-world".into()),
            id: None,
            path: None,
            include_sidecar: false,
        };
        let out = handle(vault.path(), input).unwrap();
        assert_eq!(out.id, "01JRZK3M7PQNX8BABCDE12345");
        assert_eq!(out.title, "Hello World");
        assert!(out.body.contains("This is the body."));
        assert!(out.sidecar.is_none());
        assert!(out.backlinks.is_empty());
        // frontmatter should include the title key
        assert_eq!(out.frontmatter["title"], "Hello World");
    }

    #[test]
    fn read_by_id_resolves() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            id: Some("01JRZK3M7PQNX8BABCDE12345".into()),
            slug: None,
            path: None,
            include_sidecar: false,
        };
        let out = handle(vault.path(), input).unwrap();
        assert_eq!(out.title, "Hello World");
    }

    #[test]
    fn read_by_path_resolves() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            path: Some("hello-world.md".into()),
            id: None,
            slug: None,
            include_sidecar: false,
        };
        let out = handle(vault.path(), input).unwrap();
        assert_eq!(out.title, "Hello World");
    }

    #[test]
    fn missing_note_returns_not_found_error() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            slug: Some("nope".into()),
            id: None,
            path: None,
            include_sidecar: false,
        };
        let err = handle(vault.path(), input).unwrap_err();
        assert_eq!(err.code, "not_found");
    }

    #[test]
    fn no_key_returns_bad_input() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            id: None,
            slug: None,
            path: None,
            include_sidecar: false,
        };
        let err = handle(vault.path(), input).unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn ambiguous_key_returns_bad_input() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            id: Some("01JRZK3M7PQNX8BABCDE12345".into()),
            slug: Some("hello-world".into()),
            path: None,
            include_sidecar: false,
        };
        let err = handle(vault.path(), input).unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn output_schema_includes_all_required_fields() {
        let vault = fixture_vault();
        let input = ReadNoteInput {
            slug: Some("hello-world".into()),
            id: None,
            path: None,
            include_sidecar: false,
        };
        let out = handle(vault.path(), input).unwrap();
        let json = serde_json::to_value(&out).unwrap();
        for field in ["id", "title", "path", "body", "frontmatter", "backlinks"] {
            assert!(json.get(field).is_some(), "missing field: {field}");
        }
    }
}
