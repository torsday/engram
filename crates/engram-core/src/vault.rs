//! Vault filesystem layout and note read/write operations.
//!
//! The vault is an Obsidian-compatible directory of markdown files. Each note
//! carries YAML frontmatter (`---\n…\n---`) with at least an `id` (ULID) and a
//! `title`. Sidecar JSON lives under `.engram/sidecar/<id>.json`.
//!
//! This module provides **read-only** access for now. Writes are gated behind
//! agent confidence and the review queue (ADR 0004).

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::frontmatter::{parse_frontmatter, Frontmatter, FrontmatterError};
use crate::note_id::NoteId;
use crate::sidecar::{read_sidecar, Sidecar, SidecarError};
use crate::slug::slugify;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from vault read operations.
#[derive(Debug, Error)]
pub enum VaultError {
    /// The vault root directory does not exist or is not a directory.
    #[error("vault root does not exist or is not a directory: {0}")]
    NotADirectory(PathBuf),

    /// No note matched the given lookup key.
    #[error("note not found: {0}")]
    NotFound(String),

    /// I/O error while reading a file.
    #[error("I/O error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Frontmatter parse failed.
    #[error("frontmatter error in {path}: {source}")]
    Frontmatter {
        path: PathBuf,
        #[source]
        source: FrontmatterError,
    },

    /// Sidecar read failed (non-fatal — callers may ignore).
    #[error("sidecar error: {source}")]
    Sidecar {
        #[source]
        source: SidecarError,
    },
}

// ---------------------------------------------------------------------------
// NoteRecord — the return type for read_note
// ---------------------------------------------------------------------------

/// A fully-read note from the vault.
#[derive(Debug, Clone)]
pub struct NoteRecord {
    /// Absolute filesystem path to the `.md` file.
    pub path: PathBuf,

    /// Note identifier (ULID), as parsed from frontmatter.
    pub id: String,

    /// Note title, as parsed from frontmatter.
    pub title: String,

    /// Parsed frontmatter.
    pub frontmatter: Frontmatter,

    /// Note body (everything after the closing `---`).
    pub body: String,

    /// Sidecar JSON, if present and `include_sidecar` was requested.
    pub sidecar: Option<Sidecar>,

    /// Backlinks (notes that link to this note).
    ///
    /// Populated only when a live link-graph index is available.
    /// Returns an empty vec when the link graph hasn't been built yet.
    pub backlinks: Vec<Backlink>,
}

/// One resolved backlink: a note that links to the target note.
#[derive(Debug, Clone)]
pub struct Backlink {
    /// ULID of the linking note.
    pub from_note_id: String,
    /// The anchor text used in the wikilink (or the link target if no alias).
    pub anchor: String,
}

// ---------------------------------------------------------------------------
// Lookup key — by ULID id, slug, or path
// ---------------------------------------------------------------------------

/// How to locate a note inside the vault.
#[derive(Debug, Clone)]
pub enum NoteKey<'a> {
    /// Look up by ULID string (the `id:` frontmatter field).
    Id(&'a str),
    /// Look up by title slug (e.g. `"my-note-title"`).
    Slug(&'a str),
    /// Look up by path relative to the vault root (e.g. `"journal/2026-05-20.md"`).
    Path(&'a str),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read a single note from the vault, resolved by the given [`NoteKey`].
///
/// `vault_root` is the directory that contains the `.md` files.
/// `include_sidecar` controls whether the `.engram/sidecar/<id>.json` file is
/// loaded and attached to the returned record.
///
/// Backlinks are always empty in this implementation — they require a live
/// [`engram_index`] link-graph query that is not yet wired here.
pub fn read_note(
    vault_root: &Path,
    key: NoteKey<'_>,
    include_sidecar: bool,
) -> Result<NoteRecord, VaultError> {
    if !vault_root.is_dir() {
        return Err(VaultError::NotADirectory(vault_root.to_path_buf()));
    }

    let md_path = resolve_path(vault_root, &key)?;
    let raw = std::fs::read_to_string(&md_path).map_err(|e| VaultError::Io {
        path: md_path.clone(),
        source: e,
    })?;

    let frontmatter = parse_frontmatter(&raw).map_err(|e| VaultError::Frontmatter {
        path: md_path.clone(),
        source: e,
    })?;

    let body = extract_body(&raw);

    let sidecar = if include_sidecar {
        match NoteId::parse(&frontmatter.id) {
            Ok(note_id) => match read_sidecar(&note_id, vault_root) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::debug!("sidecar not found or unreadable: {e}");
                    None
                }
            },
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(NoteRecord {
        id: frontmatter.id.clone(),
        title: frontmatter.title.clone(),
        path: md_path,
        frontmatter,
        body,
        sidecar,
        backlinks: vec![],
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve a [`NoteKey`] to an absolute `.md` path within `vault_root`.
fn resolve_path(vault_root: &Path, key: &NoteKey<'_>) -> Result<PathBuf, VaultError> {
    match key {
        NoteKey::Path(rel) => {
            let p = vault_root.join(rel);
            if p.is_file() {
                Ok(p)
            } else {
                Err(VaultError::NotFound(rel.to_string()))
            }
        }

        NoteKey::Id(id) => {
            // Walk the vault looking for a note whose `id:` field matches.
            find_note_by(vault_root, |fm| fm.id == *id)
                .ok_or_else(|| VaultError::NotFound(format!("id={id}")))
        }

        NoteKey::Slug(slug) => {
            // Fast path: check if a file named `<slug>.md` exists in the root.
            let candidate = vault_root.join(format!("{slug}.md"));
            if candidate.is_file() {
                return Ok(candidate);
            }
            // Slow path: walk and compare slugified titles.
            find_note_by(vault_root, |fm| slugify(&fm.title) == *slug)
                .ok_or_else(|| VaultError::NotFound(format!("slug={slug}")))
        }
    }
}

/// Walk `vault_root` (non-hidden files only) and return the path of the first
/// `.md` file whose frontmatter satisfies `pred`.
fn find_note_by<F>(vault_root: &Path, pred: F) -> Option<PathBuf>
where
    F: Fn(&Frontmatter) -> bool,
{
    walk_md_files(vault_root, |path| {
        let raw = std::fs::read_to_string(path).ok()?;
        let fm = parse_frontmatter(&raw).ok()?;
        if pred(&fm) {
            Some(path.to_path_buf())
        } else {
            None
        }
    })
}

/// Walk `.md` files under `vault_root`, skipping hidden directories.
/// Calls `f` for each file and returns the first `Some(T)` result.
fn walk_md_files<F, T>(vault_root: &Path, f: F) -> Option<T>
where
    F: Fn(&Path) -> Option<T>,
{
    walk_dir_inner(vault_root, &f)
}

fn walk_dir_inner<F, T>(dir: &Path, f: &F) -> Option<T>
where
    F: Fn(&Path) -> Option<T>,
{
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden directories (`.git`, `.engram`, etc.)
        if name_str.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            if let Some(result) = walk_dir_inner(&path, f) {
                return Some(result);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            if let Some(result) = f(&path) {
                return Some(result);
            }
        }
    }
    None
}

/// Extract the note body: everything after the closing `---` of the frontmatter.
fn extract_body(content: &str) -> String {
    // The frontmatter block is `---\n...\n---\n`. Skip the first and second `---`.
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return content.to_owned();
    }
    let rest: Vec<&str> = lines.collect();
    let close = rest.iter().position(|l| l.trim() == "---");
    match close {
        None => content.to_owned(),
        Some(i) => rest[i + 1..].join("\n"),
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

    fn make_vault() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let note = "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: My Test Note\ntype: evergreen\n---\n\nHello world.";
        fs::write(dir.path().join("my-test-note.md"), note).unwrap();
        dir
    }

    #[test]
    fn read_by_slug_filename() {
        let vault = make_vault();
        let rec = read_note(vault.path(), NoteKey::Slug("my-test-note"), false).unwrap();
        assert_eq!(rec.title, "My Test Note");
        assert_eq!(rec.body.trim(), "Hello world.");
        assert!(rec.sidecar.is_none());
        assert!(rec.backlinks.is_empty());
    }

    #[test]
    fn read_by_path() {
        let vault = make_vault();
        let rec = read_note(vault.path(), NoteKey::Path("my-test-note.md"), false).unwrap();
        assert_eq!(rec.id, "01JRZK3M7PQNX8BABCDE12345");
    }

    #[test]
    fn read_by_id() {
        let vault = make_vault();
        let rec = read_note(
            vault.path(),
            NoteKey::Id("01JRZK3M7PQNX8BABCDE12345"),
            false,
        )
        .unwrap();
        assert_eq!(rec.title, "My Test Note");
    }

    #[test]
    fn read_missing_returns_not_found() {
        let vault = make_vault();
        let err = read_note(vault.path(), NoteKey::Slug("no-such-note"), false).unwrap_err();
        assert!(matches!(err, VaultError::NotFound(_)));
    }

    #[test]
    fn read_invalid_vault_root() {
        let err =
            read_note(Path::new("/no/such/dir"), NoteKey::Slug("anything"), false).unwrap_err();
        assert!(matches!(err, VaultError::NotADirectory(_)));
    }

    #[test]
    fn body_extraction() {
        let content = "---\nid: X\ntitle: T\ntype: fleeting\n---\n\nBody here.\n";
        let body = extract_body(content);
        assert_eq!(body.trim(), "Body here.");
    }
}
