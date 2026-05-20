//! Wire-shape types used by every [`crate::LlmProvider`] implementation.
//!
//! Kept deliberately small: the trait is a thin abstraction over completion
//! and embedding endpoints. Provider-specific concerns (Anthropic's tool-use
//! blocks, OpenAI's function calls, Ollama's local-only options) belong in
//! the concrete provider modules, not here.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

// ─── Price table ─────────────────────────────────────────────────────────────

/// Raw per-model entry as parsed from `prices.toml`.
#[derive(Debug, Deserialize)]
struct PriceEntry {
    name: String,
    input_cents_per_million_tokens: f64,
    cache_read_cents_per_million_tokens: f64,
    cache_creation_cents_per_million_tokens: f64,
    output_cents_per_million_tokens: f64,
}

/// Top-level shape of `prices.toml`.
#[derive(Debug, Deserialize)]
struct PriceFile {
    models: Vec<PriceEntry>,
}

/// Compiled price table keyed by model name.
type PriceMap = HashMap<String, PriceEntry>;

static PRICE_MAP: OnceLock<PriceMap> = OnceLock::new();

fn price_map() -> &'static PriceMap {
    PRICE_MAP.get_or_init(|| {
        let raw = include_str!("../prices.toml");
        let file: PriceFile =
            toml::from_str(raw).expect("prices.toml is malformed — this is a compile-time asset");
        file.models
            .into_iter()
            .map(|e| (e.name.clone(), e))
            .collect()
    })
}

// ─── Cost ────────────────────────────────────────────────────────────────────

/// Per-call cost computed against the static price table.
///
/// All amounts are in US cents (not dollars) to avoid sub-cent floating-point
/// precision loss for typical call sizes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// Cost of non-cached input tokens.
    pub input_cents: f64,
    /// Cost of cache-creation tokens (billed at the create rate).
    pub cache_create_cents: f64,
    /// Cost of cache-read tokens (billed at the discounted rate).
    pub cache_read_cents: f64,
    /// Cost of output tokens.
    pub output_cents: f64,
    /// Sum of all four components.
    pub total_cents: f64,
}

impl Cost {
    /// Sentinel returned when the model is unknown — all fields zero.
    pub fn unknown() -> Self {
        Self {
            input_cents: 0.0,
            cache_create_cents: 0.0,
            cache_read_cents: 0.0,
            output_cents: 0.0,
            total_cents: 0.0,
        }
    }

    /// Compute cost from a [`Usage`] report and the [`Model`] that served it.
    ///
    /// Cache-read tokens are billed at `cache_read_cents_per_million_tokens`.
    /// Cache-creation tokens are billed at `cache_creation_cents_per_million_tokens`.
    /// The remaining input tokens (total − cached − cache_create) are billed at
    /// the regular `input_cents_per_million_tokens`.
    ///
    /// If the model name is not in the price table, logs a warning (once per
    /// process per unknown name) and returns [`Cost::unknown()`].
    pub fn from_usage(usage: &Usage, model: &Model) -> Self {
        let table = price_map();
        // Strip the provider prefix if present ("anthropic/claude-...").
        let bare_name = model.name.rsplit('/').next().unwrap_or(model.name.as_str());

        let Some(entry) = table.get(bare_name) else {
            static WARNED: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
                OnceLock::new();
            let warned =
                WARNED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
            let mut guard = warned.lock().unwrap_or_else(|e| e.into_inner());
            if guard.insert(bare_name.to_owned()) {
                tracing::warn!(
                    model = %model.name,
                    "unknown model in price table — reporting Cost::unknown()"
                );
            }
            return Self::unknown();
        };

        const M: f64 = 1_000_000.0;

        let cache_read = usage.input_tokens_cached as f64;
        let cache_create = usage.input_tokens_cache_create as f64;
        // Non-cached input = total minus cached and cache-create.
        let plain_input = (usage.input_tokens_total as f64 - cache_read - cache_create).max(0.0);
        let output = usage.output_tokens as f64;

        let input_cents = plain_input / M * entry.input_cents_per_million_tokens;
        let cache_create_cents = cache_create / M * entry.cache_creation_cents_per_million_tokens;
        let cache_read_cents = cache_read / M * entry.cache_read_cents_per_million_tokens;
        let output_cents = output / M * entry.output_cents_per_million_tokens;
        let total_cents = input_cents + cache_create_cents + cache_read_cents + output_cents;

        Self {
            input_cents,
            cache_create_cents,
            cache_read_cents,
            output_cents,
            total_cents,
        }
    }
}

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
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    /// Generated text, concatenated across the response's content blocks.
    pub text: String,
    /// Provider-reported token usage.
    pub usage: Usage,
    /// Per-call cost computed from [`usage`] against the static price table.
    pub cost: Cost,
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

    // ─── Cost tests ───────────────────────────────────────────────────────────

    #[test]
    fn cost_unknown_model_returns_zeros() {
        let usage = Usage {
            input_tokens_total: 1000,
            output_tokens: 500,
            ..Default::default()
        };
        let model = Model::anthropic("claude-does-not-exist");
        let cost = Cost::from_usage(&usage, &model);
        assert_eq!(cost.total_cents, 0.0);
        assert_eq!(cost.input_cents, 0.0);
        assert_eq!(cost.output_cents, 0.0);
    }

    #[test]
    fn cost_zero_usage_is_zero() {
        let usage = Usage::default();
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let cost = Cost::from_usage(&usage, &model);
        assert_eq!(cost.total_cents, 0.0);
        assert_eq!(
            cost.total_cents,
            cost.input_cents + cost.cache_create_cents + cost.cache_read_cents + cost.output_cents
        );
    }

    #[test]
    fn cost_plain_input_and_output_hand_computed() {
        // claude-3-5-haiku-20241022:
        //   input  = 80 cents / M tokens
        //   output = 400 cents / M tokens
        // 1M input + 1M output = 80 + 400 = 480 cents
        let usage = Usage {
            input_tokens_total: 1_000_000,
            input_tokens_cached: 0,
            input_tokens_cache_create: 0,
            output_tokens: 1_000_000,
        };
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let cost = Cost::from_usage(&usage, &model);
        let eps = 1e-9;
        assert!(
            (cost.input_cents - 80.0).abs() < eps,
            "input_cents={}",
            cost.input_cents
        );
        assert!(
            (cost.output_cents - 400.0).abs() < eps,
            "output_cents={}",
            cost.output_cents
        );
        assert!(
            (cost.total_cents - 480.0).abs() < eps,
            "total_cents={}",
            cost.total_cents
        );
    }

    #[test]
    fn cost_cached_token_discount_applied() {
        // claude-3-5-haiku-20241022:
        //   cache_read = 8 cents / M tokens (10% of input rate)
        // 500k cached reads only, no plain input, no output.
        let usage = Usage {
            input_tokens_total: 500_000,
            input_tokens_cached: 500_000,
            input_tokens_cache_create: 0,
            output_tokens: 0,
        };
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let cost = Cost::from_usage(&usage, &model);
        // 0.5M * 8 cents/M = 4 cents
        let eps = 1e-9;
        assert!(
            (cost.cache_read_cents - 4.0).abs() < eps,
            "cache_read_cents={}",
            cost.cache_read_cents
        );
        assert_eq!(cost.input_cents, 0.0);
        assert!((cost.total_cents - 4.0).abs() < eps);
    }

    #[test]
    fn cost_cache_create_rate() {
        // claude-3-5-haiku-20241022:
        //   cache_creation = 100 cents / M tokens
        // 1M cache-create tokens = 100 cents.
        let usage = Usage {
            input_tokens_total: 1_000_000,
            input_tokens_cached: 0,
            input_tokens_cache_create: 1_000_000,
            output_tokens: 0,
        };
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let cost = Cost::from_usage(&usage, &model);
        let eps = 1e-9;
        assert!(
            (cost.cache_create_cents - 100.0).abs() < eps,
            "cache_create_cents={}",
            cost.cache_create_cents
        );
        assert_eq!(cost.input_cents, 0.0);
        assert!((cost.total_cents - 100.0).abs() < eps);
    }
}
