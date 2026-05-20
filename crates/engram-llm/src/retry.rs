//! Retry decorator for [`crate::LlmProvider`].
//!
//! Wraps any concrete provider in a policy that:
//!
//! - Re-invokes the underlying call on [`engram_core::error::ErrorCategory::Transient`]
//!   failures, up to `RetryConfig::max_attempts` (including the initial try)
//!   or until the total wall-clock budget elapses.
//! - Backs off exponentially with multiplicative jitter — base `1s`, factor
//!   `2`, jitter uniform on `[0.5, 1.5]`, capped at `max_delay`.
//! - Returns [`crate::Error::RetryBudgetExhausted`] when the policy gives up
//!   on a transient run, carrying the last underlying failure's `Display`.
//! - Passes non-transient errors through unchanged (fail fast).
//!
//! # Composition
//!
//! Per the issue spec, stack the breaker outside the retry wrapper:
//!
//! ```text
//! CircuitBreakerProvider::new(RetryProvider::new(AnthropicProvider))
//! ```
//!
//! The retry layer is the inner layer so individual transient hiccups don't
//! count against the breaker's threshold (only a fully-exhausted retry budget
//! does).
//!
//! # Streaming
//!
//! Streamed calls are retried only on the initial request — once the stream
//! starts flowing, mid-stream failures bubble up unchanged. Retrying a
//! partially-consumed stream would require replaying delivered deltas, which
//! the caller (agent runtime) can do more cleanly than this layer.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use rand::Rng;

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::StreamedCompletion;
use crate::types::{CompleteOptions, Completion, EmbeddingModel, Model, PromptStructured};

/// Tunable retry policy. Defaults match `docs/design/03-architecture.md`
/// §LLM call retry policy: 4 attempts (1 + 3 retries), 1s base, max 30s
/// between, 60s total budget.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of attempts, including the first. `4` means three
    /// retries after the initial call.
    pub max_attempts: u32,
    /// Base backoff applied before any jitter. The actual delay before
    /// attempt `n` (1-indexed) is `base * 2^(n-1) * jitter`.
    pub base: Duration,
    /// Backoff multiplier — fixed at `2` for the standard policy. Held as
    /// a field so tests can shrink it.
    pub factor: u32,
    /// Hard cap on the post-jitter delay between attempts.
    pub max_delay: Duration,
    /// Total wall-clock budget across all attempts (including waits).
    pub total_budget: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base: Duration::from_secs(1),
            factor: 2,
            max_delay: Duration::from_secs(30),
            total_budget: Duration::from_secs(60),
        }
    }
}

/// A [`LlmProvider`] decorator that retries transient failures.
///
/// See module docs for the policy and composition rules.
pub struct RetryProvider<P: LlmProvider> {
    inner: P,
    config: RetryConfig,
}

impl<P: LlmProvider> RetryProvider<P> {
    /// Wrap `inner` with the default retry policy.
    pub fn new(inner: P) -> Self {
        Self::with_config(inner, RetryConfig::default())
    }

    /// Wrap `inner` with an explicit policy. Used by tests with shortened
    /// timers.
    pub fn with_config(inner: P, config: RetryConfig) -> Self {
        Self { inner, config }
    }

    /// Borrow the underlying provider (escape hatch for tests; production
    /// code should never need this).
    pub fn inner(&self) -> &P {
        &self.inner
    }
}

/// Run `op` under the configured retry policy.
///
/// Generic over the operation closure so all three trait methods share the
/// same loop. Each attempt re-invokes the closure; on transient failure we
/// sleep for the jittered backoff and retry, on non-transient we return
/// immediately, on budget exhaustion we surface a
/// [`Error::RetryBudgetExhausted`].
async fn run_with_retry<T, F, Fut>(config: &RetryConfig, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let started = Instant::now();
    let mut last_err: Option<Error> = None;

    for attempt in 1..=config.max_attempts {
        if started.elapsed() >= config.total_budget {
            break;
        }

        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let category = e.category();
                if !category.is_retryable() {
                    return Err(e);
                }
                // If the provider gave us a Retry-After hint, respect it
                // (capped at max_delay so a hostile header can't hold us hostage).
                let provider_hint = match &e {
                    Error::RateLimited {
                        retry_after: Some(d),
                        ..
                    } => Some(*d),
                    _ => None,
                };

                // Decide backoff before consuming `e` into `last_err`.
                let next_attempt = attempt + 1;
                let jittered_backoff = compute_backoff(config, attempt);
                // Use the provider hint when it is larger than the jittered
                // backoff, but cap at max_delay so it cannot stall us forever.
                let backoff = match provider_hint {
                    Some(hint) => hint.max(jittered_backoff).min(config.max_delay),
                    None => jittered_backoff,
                };

                tracing::warn!(
                    attempt,
                    next_attempt = if next_attempt <= config.max_attempts {
                        Some(next_attempt)
                    } else {
                        None
                    },
                    category = %category,
                    backoff_ms = backoff.as_millis() as u64,
                    provider_hint_ms = provider_hint.map(|d| d.as_millis() as u64),
                    error = %e,
                    "engram-llm retry: transient failure"
                );
                last_err = Some(e);

                if attempt == config.max_attempts {
                    break;
                }

                let remaining = config.total_budget.saturating_sub(started.elapsed());
                let sleep_for = backoff.min(remaining);
                if sleep_for.is_zero() {
                    break;
                }
                tokio::time::sleep(sleep_for).await;
            }
        }
    }

    let last = last_err
        .as_ref()
        .map(|e| e.to_string())
        .unwrap_or_else(|| "no underlying error captured".to_string());
    tracing::error!(
        attempts = config.max_attempts,
        elapsed_ms = started.elapsed().as_millis() as u64,
        last,
        "engram-llm retry: budget exhausted"
    );
    Err(Error::RetryBudgetExhausted {
        attempts: config.max_attempts,
        last,
    })
}

/// Standalone copy of [`RetryProvider::backoff`] usable inside the generic
/// retry loop (which has no `self`). Behaviour is identical.
fn compute_backoff(config: &RetryConfig, attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(20);
    let factor_pow = (config.factor as u64).saturating_pow(exp);
    let unjittered = config.base.saturating_mul(factor_pow as u32);
    let capped = unjittered.min(config.max_delay);
    let jitter = rand::thread_rng().gen_range(0.5..1.5);
    let jittered_nanos = (capped.as_nanos() as f64 * jitter) as u64;
    Duration::from_nanos(jittered_nanos).min(config.max_delay)
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for RetryProvider<P> {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        run_with_retry(&self.config, || self.inner.complete(prompt, model, options)).await
    }

    async fn complete_streamed(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        // Retry only the *initial* request — see module docs.
        run_with_retry(&self.config, || {
            self.inner.complete_streamed(prompt, model, options)
        })
        .await
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        run_with_retry(&self.config, || self.inner.embed(text, model)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially_within_jitter_band() {
        let cfg = RetryConfig {
            base: Duration::from_millis(100),
            factor: 2,
            max_delay: Duration::from_secs(60),
            ..Default::default()
        };

        // The unjittered value for attempt n is base * 2^(n-1).
        // The jitter multiplier is [0.5, 1.5], so the observed delay must
        // lie in [unjittered * 0.5, unjittered * 1.5], capped at max_delay.
        for attempt in 1..=4 {
            let unjittered_ms = 100u64 * 2u64.pow(attempt - 1);
            let lo = (unjittered_ms as f64 * 0.5) as u64;
            let hi = (unjittered_ms as f64 * 1.5) as u64;
            // Sample many times — jitter is randomised per call.
            for _ in 0..100 {
                let d = compute_backoff(&cfg, attempt).as_millis() as u64;
                assert!(
                    d >= lo && d <= hi,
                    "attempt {attempt}: backoff {d}ms outside [{lo}, {hi}]"
                );
            }
        }
    }

    #[test]
    fn backoff_respects_max_delay_cap() {
        let cfg = RetryConfig {
            base: Duration::from_secs(60),
            factor: 2,
            max_delay: Duration::from_secs(30),
            ..Default::default()
        };
        // Even at attempt 1 the unjittered value (60s) exceeds the cap, so
        // every sample must be ≤ 30s.
        for _ in 0..100 {
            let d = compute_backoff(&cfg, 1);
            assert!(
                d <= Duration::from_secs(30),
                "got {}ms, cap is 30000ms",
                d.as_millis()
            );
        }
    }
}
