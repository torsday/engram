//! Cross-crate error classification taxonomy.
//!
//! Every engram crate defines its own concrete `Error` enum, but the
//! resilience layer (retry + circuit-breaker + timeout, see
//! `docs/design/03-architecture.md` §Error handling and resilience) needs a
//! shared vocabulary for deciding what to do with a failure. That vocabulary
//! lives here.
//!
//! # Categories
//!
//! [`ErrorCategory`] partitions failures into four classes:
//!
//! - [`ErrorCategory::Transient`] — try again, the world might recover (network
//!   blip, 5xx, rate-limit, half-open trial). Retry policy applies.
//! - [`ErrorCategory::Permanent`] — won't recover by retrying (4xx other than
//!   429, context overflow, schema-mismatch on the request). Fail fast.
//! - [`ErrorCategory::System`] — engram itself is broken (config invalid,
//!   secret missing, internal invariant violated). Surface to the operator;
//!   do not retry.
//! - [`ErrorCategory::External`] — a dependency is broken in a way the retry
//!   wrapper should not paper over (upstream API contract change, provider
//!   account suspended). Bubble up.
//!
//! Each concrete `Error` exposes a `fn category(&self) -> ErrorCategory` so
//! the retry layer can branch without re-implementing the classification
//! per crate.

use std::fmt;

/// Classification of an error for retry / circuit-breaker decisions.
///
/// Every concrete error in engram maps onto exactly one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// Transient failure — safe to retry under the standard policy.
    ///
    /// Examples: TCP reset, DNS timeout, HTTP 5xx, HTTP 429, empty response,
    /// circuit-breaker half-open trial that didn't return in time.
    Transient,

    /// Permanent failure — retrying won't help. Fail fast.
    ///
    /// Examples: HTTP 400/401/403 (not 429), context-window overflow,
    /// schema-validation rejection on the request body itself.
    Permanent,

    /// Operator / configuration problem — engram is mis-set-up.
    ///
    /// Examples: provider API key missing, config file invalid TOML, expected
    /// vault file absent.
    System,

    /// External dependency contract broken in a way the retry layer should
    /// not silently paper over.
    ///
    /// Examples: provider returned a payload our parser cannot decode (likely
    /// upstream wire-format change), provider account suspended (403 with a
    /// well-known body).
    External,
}

impl ErrorCategory {
    /// Whether the standard retry policy should attempt this error again.
    ///
    /// Returns `true` only for [`Self::Transient`]; every other category is
    /// terminal as far as retry is concerned. Circuit-breaker state machines
    /// also use this to decide whether a failure counts toward the open
    /// threshold (it does only for `Transient` and `External`).
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Transient)
    }

    /// Whether a failure of this category should increment the
    /// circuit-breaker failure counter.
    ///
    /// Operator / config errors don't open the breaker — the breaker is for
    /// upstream-health protection, not engram's own bugs.
    pub fn counts_toward_breaker(self) -> bool {
        matches!(self, Self::Transient | Self::External)
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Transient => "transient",
            Self::Permanent => "permanent",
            Self::System => "system",
            Self::External => "external",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_is_retryable_only() {
        assert!(ErrorCategory::Transient.is_retryable());
        assert!(!ErrorCategory::Permanent.is_retryable());
        assert!(!ErrorCategory::System.is_retryable());
        assert!(!ErrorCategory::External.is_retryable());
    }

    #[test]
    fn only_transient_and_external_open_the_breaker() {
        assert!(ErrorCategory::Transient.counts_toward_breaker());
        assert!(ErrorCategory::External.counts_toward_breaker());
        assert!(!ErrorCategory::Permanent.counts_toward_breaker());
        assert!(!ErrorCategory::System.counts_toward_breaker());
    }

    #[test]
    fn display_matches_lowercase_name() {
        assert_eq!(ErrorCategory::Transient.to_string(), "transient");
        assert_eq!(ErrorCategory::Permanent.to_string(), "permanent");
        assert_eq!(ErrorCategory::System.to_string(), "system");
        assert_eq!(ErrorCategory::External.to_string(), "external");
    }
}
