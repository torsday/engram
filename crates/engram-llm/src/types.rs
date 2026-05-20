//! Wire-shape types used by every [`crate::LlmProvider`] implementation.
//!
//! Kept deliberately small: the trait is a thin abstraction over completion
//! and embedding endpoints. Provider-specific concerns (Anthropic's tool-use
//! blocks, OpenAI's function calls, Ollama's local-only options) belong in
//! the concrete provider modules, not here.

use serde::{Deserialize, Serialize};

/// Which provider serves a given [`Model`].
///
/// New providers (OpenAI, Ollama) extend this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProvider {
    /// Anthropic Claude (Messages API).
    Anthropic,
    /// Reserved for #20.
    OpenAi,
    /// Reserved for the local-fallback provider.
    Ollama,
}

/// A concrete model selection: provider + model identifier.
///
/// Caller-supplied per call so the runtime can tier-escalate from Haiku to
/// Sonnet to Opus on a single agent without rebuilding the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// Which provider serves this model.
    pub provider: ModelProvider,
    /// Provider-specific model identifier (e.g. `claude-3-5-haiku-20241022`).
    pub name: String,
}

impl Model {
    /// Build an Anthropic [`Model`] with the given identifier.
    pub fn anthropic(name: impl Into<String>) -> Self {
        Self {
            provider: ModelProvider::Anthropic,
            name: name.into(),
        }
    }
}

/// A concrete embedding model: provider + model identifier + output
/// dimensionality.
///
/// Dimensionality is part of the type so consumers (LanceDB index, cosine
/// similarity computations) cannot accidentally mix vectors of different
/// dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModel {
    /// Which provider serves this model.
    pub provider: ModelProvider,
    /// Provider-specific model identifier (e.g. `voyage-3-large`).
    pub name: String,
    /// Output dimensionality.
    pub dim: usize,
}

/// A prompt split for cache-friendly transport. Per ADR 0010.
///
/// - `static_head` — identical across many calls within ~5 minutes. The
///   provider inserts a cache-control marker at its boundary.
/// - `dynamic_tail` — varies per call. Never cached.
///
/// The split is purely structural; it has no semantic meaning to the model
/// (it sees both halves as one user message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptStructured {
    /// Static prefix; provider emits a cache marker at its end.
    pub static_head: String,
    /// Dynamic suffix; never cached.
    pub dynamic_tail: String,
}

impl PromptStructured {
    /// Build a `PromptStructured` from the two halves.
    pub fn new(static_head: impl Into<String>, dynamic_tail: impl Into<String>) -> Self {
        Self {
            static_head: static_head.into(),
            dynamic_tail: dynamic_tail.into(),
        }
    }

    /// Build a degenerate `PromptStructured` with an empty static head.
    /// Useful for prompts not yet structured for caching — token cost is
    /// the same as before.
    pub fn dynamic_only(prompt: impl Into<String>) -> Self {
        Self {
            static_head: String::new(),
            dynamic_tail: prompt.into(),
        }
    }
}

/// Tunable per-call options shared across providers.
///
/// Fields here are the lowest common denominator across the four supported
/// providers; provider-specific knobs (Anthropic's `top_k`, Ollama's `seed`)
/// belong on the concrete provider's builder, not here.
#[derive(Debug, Clone, PartialEq)]
pub struct CompleteOptions {
    /// Sampling temperature in `[0.0, 1.0]`. Defaults to `0.2` — engram is
    /// almost never well-served by high entropy.
    pub temperature: f32,
    /// Maximum tokens the provider may generate. Defaults to `1024`.
    pub max_tokens: u32,
    /// Stop sequences. Empty means "no stop sequences."
    pub stop_sequences: Vec<String>,
    /// Optional system prompt. Anthropic places this in the top-level
    /// `system` field rather than as a message; other providers may do
    /// otherwise.
    pub system: Option<String>,
}

impl Default for CompleteOptions {
    fn default() -> Self {
        Self {
            temperature: 0.2,
            max_tokens: 1024,
            stop_sequences: Vec::new(),
            system: None,
        }
    }
}

/// Token-accounting summary returned with every completion.
///
/// `input_tokens_cached` is the share of input tokens that hit the provider's
/// cache and were billed at the reduced rate. Anthropic exposes this via
/// `usage.cache_read_input_tokens`; providers without caching report `0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Total input tokens for this call.
    pub input_tokens_total: u32,
    /// Input tokens served from the provider's cache.
    pub input_tokens_cached: u32,
    /// Input tokens billed at the cache-creation rate (first-time-write).
    pub input_tokens_cache_create: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
}

impl Usage {
    /// Fraction of input tokens served from cache. `0.0` if `input_tokens_total == 0`.
    pub fn cache_hit_ratio(&self) -> f32 {
        if self.input_tokens_total == 0 {
            0.0
        } else {
            self.input_tokens_cached as f32 / self.input_tokens_total as f32
        }
    }
}

/// A successful completion. The `text` is the assistant's reply concatenated
/// across content blocks; agents that want structured output parse JSON out
/// of it themselves (the trait deliberately stays untyped — see crate-level
/// note on object-safety).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Generated text, concatenated across the response's content blocks.
    pub text: String,
    /// Provider-reported token usage.
    pub usage: Usage,
    /// Model that actually served the request (`provider/model_name`),
    /// recorded for traces and post-hoc tier-escalation analysis.
    pub model_used: String,
    /// Wall-clock latency observed by the caller, in milliseconds.
    pub latency_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_options_default() {
        let o = CompleteOptions::default();
        assert!((o.temperature - 0.2).abs() < f32::EPSILON);
        assert_eq!(o.max_tokens, 1024);
        assert!(o.stop_sequences.is_empty());
        assert!(o.system.is_none());
    }

    #[test]
    fn usage_cache_hit_ratio() {
        let u = Usage {
            input_tokens_total: 1000,
            input_tokens_cached: 800,
            ..Default::default()
        };
        assert!((u.cache_hit_ratio() - 0.8).abs() < f32::EPSILON);

        let zero = Usage::default();
        assert_eq!(zero.cache_hit_ratio(), 0.0);
    }

    #[test]
    fn prompt_structured_dynamic_only() {
        let p = PromptStructured::dynamic_only("hello");
        assert!(p.static_head.is_empty());
        assert_eq!(p.dynamic_tail, "hello");
    }

    #[test]
    fn model_anthropic_helper() {
        let m = Model::anthropic("claude-3-5-haiku-20241022");
        assert_eq!(m.provider, ModelProvider::Anthropic);
        assert_eq!(m.name, "claude-3-5-haiku-20241022");
    }
}
