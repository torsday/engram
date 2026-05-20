//! Streaming-completion types for [`crate::LlmProvider::complete_streamed`].
//!
//! Consumers receive a `Stream<Item = Result<StreamChunk>>`. The terminal
//! [`StreamChunk::Done`] carries the final usage / latency / `model_used`;
//! after it, the stream yields nothing.
//!
//! Provider-side SSE parsing lives in the concrete provider module
//! (`crate::anthropic::stream`); this module is provider-agnostic.

use std::pin::Pin;

use futures_util::Stream;

use crate::error::Result;
use crate::types::{Cost, Usage};

/// Boxed stream of streaming-completion chunks. The trait method returns
/// this so the trait stays object-safe.
pub type StreamedCompletion = Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>;

/// One increment on a streamed completion.
///
/// The lifecycle: zero or more [`StreamChunk::Delta`] events, optionally
/// interleaved with [`StreamChunk::ToolUseMarker`] forward-compat markers,
/// followed by exactly one [`StreamChunk::Done`]. Implementations must not
/// emit a `Done` more than once; consumers may stop polling after the first
/// `Done`.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    /// Incremental text delta. Append to the running response text.
    Delta {
        /// Text fragment to append.
        text: String,
    },

    /// Forward-compatibility marker for tool-use blocks. Carries no payload
    /// in this slice; the full tool-use surface lands with the tool-use
    /// follow-up. Consumers that don't yet understand tool-use should treat
    /// this as a no-op rather than an error.
    ToolUseMarker,

    /// Stream complete. Carries the final usage / provenance.
    Done {
        /// Final token usage. `output_tokens` is only correct after `Done`.
        usage: Usage,
        /// Per-call cost computed from [`usage`] against the static price table.
        cost: Cost,
        /// Model that actually served the request (`provider/model_name`).
        model_used: String,
        /// Wall-clock latency of the streamed call (handshake to last byte),
        /// in milliseconds.
        latency_ms: u64,
    },
}
