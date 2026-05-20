//! Error taxonomy for [`crate::SecretsStore`] implementations.
//!
//! Variants distinguish the failure modes callers may reasonably want to
//! recover from (e.g. fall back to env on `NotFound`) from the ones that
//! should propagate (`Backend`, `Io`). No variant ever carries the secret
//! value.

use std::path::PathBuf;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by [`crate::SecretsStore`] methods.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested secret name does not exist in the backend.
    #[error("secret `{name}` not found")]
    NotFound {
        /// Name that was looked up (never the value).
        name: String,
    },

    /// The supplied secret name is empty or contains characters outside
    /// `[A-Za-z0-9._-]`.
    #[error("invalid secret name: {0}")]
    InvalidName(String),

    /// The backend does not support this operation. Used by the env-var
    /// backend's `set` / `remove` and by the keychain backend's `list`.
    #[error("operation not supported by {backend}: {op}")]
    Unsupported {
        /// Backend identifier (e.g. `"env"`, `"keychain"`).
        backend: &'static str,
        /// Operation that was attempted (e.g. `"set"`, `"remove"`, `"list"`).
        op: &'static str,
    },

    /// Underlying OS-keychain or secret-service error. The wrapped message
    /// is the backend's own diagnostic and never contains the secret value.
    #[error("backend `{backend}` error: {message}")]
    Backend {
        /// Backend identifier.
        backend: &'static str,
        /// Human-readable error description from the underlying API.
        message: String,
    },

    /// Audit log write failure.
    #[error("audit log write failed: {path}: {source}")]
    Audit {
        /// Path of the audit log file.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// I/O error from store setup (creating audit log directory, etc.).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
