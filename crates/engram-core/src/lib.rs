//! Shared types, vault I/O, and note primitives for engram.

/// Vault filesystem layout and note read/write operations.
pub mod vault {}

/// Markdown AST parsing and structured editing via comrak.
pub mod markdown;

/// YAML frontmatter parsing and serialization (Obsidian-compatible).
pub mod frontmatter;

/// Wikilink (`[[target|alias]]`) parsing and resolution.
pub mod wikilink {}

/// Note identifiers: ULID generation and frontmatter embedding.
pub mod note_id;

/// Pure-title slug normalization (unicode fold, diacritics, path safety).
pub mod slug;

/// Filename collision detection at note-write time.
pub mod collision;

/// Sidecar JSON (`.engram/sidecar/<id>.json`) read/write.
pub mod sidecar;

/// Vault and per-agent configuration loading.
pub mod config;

/// Shared domain error types.
pub mod error;

/// Vault file watcher with debouncing, rename detection, and bounded
/// backpressure. See module docs for the full event taxonomy.
pub mod watcher;
