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
//! # Scope of this slice
//!
//! Non-streaming `complete` + `embed` for [`anthropic::AnthropicProvider`].
//! Streaming (`complete_streamed`) and per-call cost computation against a
//! static price table are filed as follow-ups; the trait reserves a place
//! for them without committing to the wire format until that slice lands.
//!
//! [ADR 0010]: ../docs/design/adrs/0010-prompt-caching-first-class.md
//! [ADR 0011]: ../docs/design/adrs/0011-tiered-model-escalation.md

#![deny(missing_docs)]

pub mod anthropic;
mod error;
mod provider;
mod types;

pub use error::{Error, Result};
pub use provider::LlmProvider;
pub use types::{
    CompleteOptions, Completion, EmbeddingModel, Model, ModelProvider, PromptStructured, Usage,
};
