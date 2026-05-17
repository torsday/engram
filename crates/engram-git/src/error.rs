//! Error taxonomy for git operations.
//!
//! Each variant maps to a distinct failure mode at the read and write surfaces
//! of the git crate. Callers match on these to distinguish I/O errors from
//! ref-resolution failures, object lookup misses, etc.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by [`crate::ReadOnlyGit`] and [`crate::WriteGit`] methods.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open repository at {path}: {message}")]
    Open { path: PathBuf, message: String },

    #[error("failed to resolve ref `{ref_name}`: {message}")]
    RevParse { ref_name: String, message: String },

    #[error("failed to read status: {0}")]
    Status(String),

    #[error("failed to compute diff: {0}")]
    Diff(String),

    #[error("failed to walk log: {0}")]
    Log(String),

    #[error("object `{sha}` not found or not a blob/tree: {message}")]
    Object { sha: String, message: String },

    #[error("commit failed: {0}")]
    Commit(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
