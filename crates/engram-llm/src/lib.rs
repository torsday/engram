//! LLM provider abstraction for engram.
//!
//! This crate defines the [`LlmProvider`] trait and ships an Anthropic
//! implementation. The trait is the runtime's single interface to large
//! language models: every agent, the embedding pipeline, and the Curator's
//! tier-escalation logic all go through it.
//!
//! # Design constraints
//!
//! - **Prompt caching is first class** per [ADR 0010]: every prompt is
//!   a [`PromptStructured`] with a `static_head` (rarely changes; provider
//!   inserts cache markers at its boundary) and a `dynamic_tail` (varies
//!   per call). The Anthropic implementation emits
//!   `cache_control: { type: "ephemeral" }` at the end of the static head.
//!
//! - **Tier escalation is first class** per [ADR 0011]: callers pass a
//!   concrete [`Model`] each call; agents start cheap (Haiku) and escalate
//!   only when their confidence gate or token-cost gate requires it.
//!
//! - **Secrets stay in the keychain** per ADR 0009 and `#15`: providers
//!   resolve their API key through `engram_secrets::SecretsStore`, never
//!   from env directly.
//!
//! # Scope
//!
//! Non-streaming `complete` + `embed` shipped in #19. Streaming
//! `complete_streamed` shipped here (#165). Per-call cost computation
//! against a static price table is filed as a separate follow-up — the
//! shape of the price table is a design decision that wants its own
//! review, and streaming is the surface that actually unblocks downstream
//! work today.
//!
//! [ADR 0010]: ../docs/design/adrs/0010-prompt-caching-first-class.md
//! [ADR 0011]: ../docs/design/adrs/0011-tiered-model-escalation.md

#![deny(missing_docs)]

pub mod anthropic;
pub mod circuit_breaker;
pub mod early_exit;
mod error;
pub mod escalating;
pub mod estimator;
#[cfg(feature = "mock-llm")]
pub mod mock;
pub mod ollama;
pub mod openai;
mod provider;
pub mod retry;
mod streaming;
pub mod timeout;
mod types;

pub use circuit_breaker::{CircuitBreakerConfig, CircuitBreakerProvider, CircuitState};
pub use early_exit::{early_exit_drive, EarlyExitConfig, EarlyExitStream, StreamedOutcome};
pub use error::{Error, Result};
pub use escalating::{EscalatingProvider, EscalationConfig, EscalationReason, SchemaValidator};
pub use estimator::{
    estimate_cost, CalibrationStore, DefaultEstimator, EstimatedCost, TokenEstimate, TokenEstimator,
};
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;
pub use provider::LlmProvider;
pub use retry::{RetryConfig, RetryProvider};
pub use streaming::{StreamChunk, StreamedCompletion};
pub use timeout::{TimeoutConfig, TimeoutProvider};
pub use types::{
    CompleteOptions, Completion, Cost, EmbeddingModel, Model, ModelProvider, PromptStructured,
    Usage,
};

/// Compose the standard resilience stack — `CircuitBreaker(Retry(Timeout(inner)))` —
/// using default budgets. Use [`CircuitBreakerProvider::with_config`],
/// [`RetryProvider::with_config`], or [`TimeoutProvider::with_config`]
/// directly when you need to tune the layers.
pub fn resilient<P: LlmProvider>(
    inner: P,
    provider_name: impl Into<String>,
) -> CircuitBreakerProvider<RetryProvider<TimeoutProvider<P>>> {
    CircuitBreakerProvider::new(
        RetryProvider::new(TimeoutProvider::new(inner)),
        provider_name,
    )
}
