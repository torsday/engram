//! The [`LlmProvider`] trait.
//!
//! The trait is object-safe via `async_trait::async_trait`, so the runtime
//! can hold a `Arc<dyn LlmProvider>` and swap providers without recompiling.
//!
//! # Object-safety / generic-output note
//!
//! The original issue spec sketches `complete<O: AgentOutput>() -> Result<O>`.
//! That signature is not object-safe — a `dyn LlmProvider` would need a
//! distinct vtable per `O`. The chosen design returns an untyped
//! [`crate::Completion`] and pushes the JSON-to-struct step into the agent
//! layer (where the schema is known anyway). Agents call
//! `serde_json::from_str::<MyOutput>(&completion.text)` themselves; the
//! trait stays small and swappable.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{CompleteOptions, Completion, EmbeddingModel, Model, PromptStructured};

/// An LLM provider — a backend that can complete prompts and embed text.
///
/// Implementations must be `Send + Sync` so the runtime can dispatch from
/// any task. All methods are async and may perform network I/O.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Complete a structured prompt non-streamingly. Returns the assembled
    /// response text plus token-usage and provenance metadata.
    ///
    /// The implementation is responsible for emitting any provider-specific
    /// prompt-cache markers at the [`PromptStructured::static_head`]/
    /// [`PromptStructured::dynamic_tail`] boundary.
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion>;

    /// Embed a single text string. Returns the vector and the actual model
    /// used (for traces).
    ///
    /// The provider validates the returned dimensionality against
    /// `model.dim` and returns [`crate::Error::Decode`] on mismatch.
    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>>;
}
