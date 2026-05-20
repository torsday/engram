//! Error taxonomy for [`crate::LlmProvider`] implementations.
//!
//! Variants distinguish the failure modes callers care about for retry +
//! escalation decisions:
//!
//! - [`Error::Secrets`] — API key resolution failed. Operator problem.
//! - [`Error::Http`] — transport-level (DNS, TCP, TLS). Retryable.
//! - [`Error::Status`] — provider returned a non-2xx. The HTTP status and
//!   any error message are preserved without the request body (which may
//!   contain prompt text or user data).
//! - [`Error::Decode`] — provider returned 2xx but the body did not match
//!   the expected schema. Programming error or upstream breakage.
//! - [`Error::Schema`] — model output did not parse to the agent's
//!   declared schema. Caller decides whether to retry, escalate, or fail.
//! - [`Error::EmptyResponse`] — provider returned a 2xx with no usable
//!   text content. Treated as a transient failure by callers.

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by [`crate::LlmProvider`] methods.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Failed to resolve the provider's API key from the secrets store.
    #[error("secrets error: {0}")]
    Secrets(#[from] engram_secrets::Error),

    /// Network or transport-level error reaching the provider.
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// Provider returned a non-2xx response. The body excerpt is the
    /// provider's `error.message` field when one was parseable; raw status
    /// otherwise. Prompt text is never included.
    #[error("provider returned status {status}: {message}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Provider-supplied error message (sanitized).
        message: String,
    },

    /// Provider returned a 2xx but the response body did not match the
    /// expected wire format.
    #[error("provider response decode error: {0}")]
    Decode(String),

    /// Successfully decoded provider response but no text content was
    /// present. Treated as transient by upstream retry logic.
    #[error("provider returned an empty response")]
    EmptyResponse,

    /// Operation not supported by this provider (e.g. `embed` on
    /// Anthropic — embeddings are served by OpenAI or Voyage).
    #[error("operation `{op}` not supported by provider `{provider}`")]
    UnsupportedByProvider {
        /// Provider identifier (e.g. `"anthropic"`).
        provider: &'static str,
        /// Operation that was attempted (`"complete"`, `"embed"`).
        op: &'static str,
    },

    /// Generic I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
