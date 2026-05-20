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
//! - [`Error::EmptyResponse`] — provider returned a 2xx with no usable
//!   text content. Treated as a transient failure by callers.
//! - [`Error::Timeout`] — the wall-clock budget for a single call expired.
//! - [`Error::RetryBudgetExhausted`] — the retry policy exhausted its
//!   attempts or total wall-clock budget; the last underlying error is
//!   carried for diagnostics.
//! - [`Error::CircuitBreakerOpen`] — the per-provider breaker is open and
//!   short-circuited the call without making a network request.
//!
//! Each variant maps onto exactly one
//! [`engram_core::error::ErrorCategory`] via [`Error::category`].

use engram_core::error::ErrorCategory;

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

    /// Provider returned HTTP 429 with an optional `Retry-After` hint.
    ///
    /// The retry decorator reads [`retry_after`] and waits *at least* that
    /// long (capped at `RetryConfig::max_delay`) before the next attempt.
    /// When the header is absent, it falls through to the default jittered
    /// backoff.
    #[error("provider rate-limited (429): {message}")]
    RateLimited {
        /// Parsed `Retry-After` duration, if the header was present and
        /// parseable. May be `Duration::ZERO` if the header said `0`.
        retry_after: Option<std::time::Duration>,
        /// Provider-supplied error message (sanitized).
        message: String,
    },

    /// Single-call wall-clock timeout. Tagged as transient — the retry
    /// wrapper will try again under the same budget.
    #[error("call timed out after {millis}ms")]
    Timeout {
        /// Configured timeout that elapsed, in milliseconds.
        millis: u64,
    },

    /// The retry policy gave up: either the per-call attempt cap or the
    /// total wall-clock budget elapsed, or the last error was non-transient.
    ///
    /// `last` carries the underlying failure as `Display` (boxed to keep the
    /// enum size sane and to side-step recursion through `Box<Self>`).
    #[error("retry budget exhausted after {attempts} attempt(s): {last}")]
    RetryBudgetExhausted {
        /// How many attempts (including the initial one) were made.
        attempts: u32,
        /// The final underlying failure, formatted for logs.
        last: String,
    },

    /// The circuit breaker for this provider is open; the call was rejected
    /// without performing any I/O. Retries should wait for the breaker to
    /// move to half-open.
    #[error("circuit breaker open for `{provider}` (cooldown remaining: {cooldown_ms}ms)")]
    CircuitBreakerOpen {
        /// Provider identifier whose breaker is open.
        provider: String,
        /// Approximate remaining cooldown until the breaker moves to
        /// half-open, in milliseconds.
        cooldown_ms: u64,
    },

    /// Generic I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Classify this error for the retry / circuit-breaker layer.
    ///
    /// The mapping is intentionally conservative: anything that might
    /// recover with another attempt is [`ErrorCategory::Transient`]; clear
    /// upstream contract breakage is [`ErrorCategory::External`]; engram
    /// configuration / setup issues are [`ErrorCategory::System`]; everything
    /// else (4xx-except-429, retry budget exhausted, breaker open) is
    /// [`ErrorCategory::Permanent`].
    pub fn category(&self) -> ErrorCategory {
        match self {
            // Operator / setup problem.
            Self::Secrets(_) => ErrorCategory::System,
            Self::Io(_) => ErrorCategory::System,
            Self::UnsupportedByProvider { .. } => ErrorCategory::System,

            // Network — almost always transient.
            Self::Http(e) => http_transient(e),

            // HTTP status — 429 / 5xx transient, 4xx-other permanent,
            // anything else outside [400, 600) treated as external.
            Self::Status { status, .. } => match *status {
                429 => ErrorCategory::Transient,
                s if (500..600).contains(&s) => ErrorCategory::Transient,
                s if (400..500).contains(&s) => ErrorCategory::Permanent,
                _ => ErrorCategory::External,
            },

            // Body didn't match our wire format → upstream contract change.
            Self::Decode(_) => ErrorCategory::External,

            Self::RateLimited { .. } => ErrorCategory::Transient,
            Self::EmptyResponse => ErrorCategory::Transient,
            Self::Timeout { .. } => ErrorCategory::Transient,

            // Once the retry policy or breaker has given up, the answer is
            // "stop, surface to the caller" — not "retry forever".
            Self::RetryBudgetExhausted { .. } => ErrorCategory::Permanent,
            Self::CircuitBreakerOpen { .. } => ErrorCategory::Permanent,
        }
    }
}

/// Classify a [`reqwest::Error`] as transient (almost always) or external
/// (the rare cases where the URL itself is malformed). Kept private — the
/// distinction is only meaningful inside [`Error::category`].
fn http_transient(e: &reqwest::Error) -> ErrorCategory {
    if e.is_builder() {
        // Malformed URL / invalid header value → caller bug, not retryable.
        ErrorCategory::Permanent
    } else {
        ErrorCategory::Transient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_5xx_is_transient() {
        let e = Error::Status {
            status: 502,
            message: "bad gateway".into(),
        };
        assert_eq!(e.category(), ErrorCategory::Transient);
    }

    #[test]
    fn http_429_is_transient() {
        let e = Error::Status {
            status: 429,
            message: "rate limited".into(),
        };
        assert_eq!(e.category(), ErrorCategory::Transient);
    }

    #[test]
    fn http_4xx_other_is_permanent() {
        for status in [400, 401, 403, 404, 422] {
            let e = Error::Status {
                status,
                message: "no".into(),
            };
            assert_eq!(
                e.category(),
                ErrorCategory::Permanent,
                "status {status} should be Permanent"
            );
        }
    }

    #[test]
    fn empty_response_is_transient() {
        assert_eq!(Error::EmptyResponse.category(), ErrorCategory::Transient);
    }

    #[test]
    fn timeout_is_transient() {
        assert_eq!(
            Error::Timeout { millis: 60_000 }.category(),
            ErrorCategory::Transient
        );
    }

    #[test]
    fn budget_exhausted_is_permanent() {
        let e = Error::RetryBudgetExhausted {
            attempts: 4,
            last: "x".into(),
        };
        assert_eq!(e.category(), ErrorCategory::Permanent);
    }

    #[test]
    fn breaker_open_is_permanent() {
        let e = Error::CircuitBreakerOpen {
            provider: "anthropic".into(),
            cooldown_ms: 30_000,
        };
        assert_eq!(e.category(), ErrorCategory::Permanent);
    }

    #[test]
    fn decode_is_external() {
        assert_eq!(
            Error::Decode("bad shape".into()).category(),
            ErrorCategory::External
        );
    }
}
