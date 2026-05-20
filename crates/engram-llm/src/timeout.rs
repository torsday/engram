//! Per-call wall-clock timeout decorator for [`crate::LlmProvider`].
//!
//! Wraps every method in `tokio::time::timeout` so a hung upstream cannot
//! stall an agent indefinitely. Different call types get different budgets,
//! matching `docs/design/03-architecture.md` §Global timeouts:
//!
//! | Call type | Default |
//! |-----------|--------:|
//! | LLM `complete` / `complete_streamed` | 60 s |
//! | Embedding | 30 s |
//!
//! Web-fetch, MCP-outbound, git-remote, and SQLite budgets live alongside
//! their respective subsystems and don't pass through this decorator.
//!
//! # Composition
//!
//! Stack the timeout layer *inside* the retry layer so each attempt has its
//! own budget rather than the whole retry session sharing one:
//!
//! ```text
//! CircuitBreakerProvider::new(RetryProvider::new(TimeoutProvider::new(AnthropicProvider)))
//! ```
//!
//! # Note on connection-layer timeouts
//!
//! The acceptance criterion calls for connection-layer timeouts (not just
//! app-level). `reqwest` is configured with its own `connect_timeout` and
//! `pool_idle_timeout` in [`crate::anthropic`]; this layer adds the
//! end-to-end ceiling.

use std::time::Duration;

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::StreamedCompletion;
use crate::types::{CompleteOptions, Completion, EmbeddingModel, Model, PromptStructured};

/// Per-call-type timeout budget. Defaults match the architecture doc.
#[derive(Debug, Clone, Copy)]
pub struct TimeoutConfig {
    /// Budget for `complete` / `complete_streamed` initial request.
    pub complete: Duration,
    /// Budget for `embed`.
    pub embed: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            complete: Duration::from_secs(60),
            embed: Duration::from_secs(30),
        }
    }
}

/// A [`LlmProvider`] decorator that enforces per-call wall-clock timeouts.
pub struct TimeoutProvider<P: LlmProvider> {
    inner: P,
    config: TimeoutConfig,
}

impl<P: LlmProvider> TimeoutProvider<P> {
    /// Wrap `inner` with the default budgets.
    pub fn new(inner: P) -> Self {
        Self::with_config(inner, TimeoutConfig::default())
    }

    /// Wrap `inner` with explicit budgets. Used by tests with shortened
    /// timers.
    pub fn with_config(inner: P, config: TimeoutConfig) -> Self {
        Self { inner, config }
    }

    /// Borrow the underlying provider.
    pub fn inner(&self) -> &P {
        &self.inner
    }
}

async fn deadline<T, Fut>(budget: Duration, fut: Fut) -> Result<T>
where
    Fut: std::future::Future<Output = Result<T>>,
{
    match tokio::time::timeout(budget, fut).await {
        Ok(inner) => inner,
        Err(_) => Err(Error::Timeout {
            millis: budget.as_millis() as u64,
        }),
    }
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for TimeoutProvider<P> {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        deadline(
            self.config.complete,
            self.inner.complete(prompt, model, options),
        )
        .await
    }

    async fn complete_streamed(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        // Only the initial-request portion is bounded; once the stream is
        // returned the deadline no longer applies (mid-stream pacing is the
        // streaming layer's concern).
        deadline(
            self.config.complete,
            self.inner.complete_streamed(prompt, model, options),
        )
        .await
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        deadline(self.config.embed, self.inner.embed(text, model)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budgets_match_design_doc() {
        let cfg = TimeoutConfig::default();
        assert_eq!(cfg.complete, Duration::from_secs(60));
        assert_eq!(cfg.embed, Duration::from_secs(30));
    }
}
