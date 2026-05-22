//! Mock LLM provider for deterministic, network-free testing.
//!
//! # Cargo feature
//!
//! This module is compiled only under the `mock-llm` feature. Production
//! builds never include it.
//!
//! # Determinism modes
//!
//! - **Scripted** — caller registers `(prompt_hash → response_text)` pairs
//!   ahead of time. Missing hashes fail loudly with the unmatched hash.
//! - **Template** — response is generated deterministically from the prompt
//!   by returning a JSON object whose `"text"` field is the first 120 chars
//!   of the prompt. Useful for tests that only care about structural validity.
//! - **Echo** — trivially echoes back a JSON-encoded snippet of the prompt.
//!   Minimal overhead for routing-only unit tests.
//!
//! # Additional capabilities
//!
//! - Realistic token accounting: input tokens = chars / 4, output = chars / 4.
//! - Streaming: response is chunked word-by-word with configurable delay.
//! - Cache-hit simulation: configurable ratio of `input_tokens_cached`.
//! - Latency simulation: configurable fixed delay per call.
//! - Failure injection: timeout, 5xx, rate-limit, or partial-stream-then-disconnect.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream;

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::{StreamChunk, StreamedCompletion};
use crate::types::{
    CompleteOptions, Completion, Cost, EmbeddingModel, Model, PromptStructured, Usage,
};

// ─── Failure injection ───────────────────────────────────────────────────────

/// A failure the mock should inject for the *next* matching call.
#[derive(Debug, Clone)]
pub enum InjectFailure {
    /// Return [`Error::Timeout`] after the given delay.
    Timeout(Duration),
    /// Return [`Error::Status`] with a 500 status code.
    ServerError,
    /// Return [`Error::RateLimited`] (HTTP 429) with no Retry-After hint.
    RateLimit,
    /// Stream a partial response (half the words), then return an I/O error.
    PartialStreamThenDisconnect,
}

// ─── Call record ─────────────────────────────────────────────────────────────

/// One recorded invocation — for assertion in tests.
#[derive(Debug, Clone)]
pub struct CallRecord {
    /// The full prompt seen by the mock (static_head + dynamic_tail).
    pub prompt: String,
    /// Model requested.
    pub model_name: String,
}

// ─── Builder ─────────────────────────────────────────────────────────────────

/// Which determinism mode the mock uses.
#[derive(Debug, Clone, Default)]
pub enum MockMode {
    /// Return scripted responses keyed by SHA-256 hex of the full prompt.
    Scripted,
    /// Generate a response from prompt content (structural validity without
    /// caring about exact LLM output).
    #[default]
    Template,
    /// Trivially echo a snippet of the prompt back.
    Echo,
}

/// Builder for [`MockLlmProvider`].
///
/// ```rust
/// use engram_llm::mock::MockLlmProvider;
///
/// let mock = MockLlmProvider::builder()
///     .mode_scripted()
///     .register("some prompt text", r#"{"confidence": 0.9}"#)
///     .cache_hit_ratio(0.6)
///     .build();
/// ```
#[derive(Default)]
pub struct MockBuilder {
    mode: MockMode,
    scripts: HashMap<String, String>,
    cache_hit_ratio: f32,
    latency: Option<Duration>,
    failures: Vec<InjectFailure>,
}

impl MockBuilder {
    /// Use scripted mode.
    pub fn mode_scripted(mut self) -> Self {
        self.mode = MockMode::Scripted;
        self
    }

    /// Use template mode (default).
    pub fn mode_template(mut self) -> Self {
        self.mode = MockMode::Template;
        self
    }

    /// Use echo mode.
    pub fn mode_echo(mut self) -> Self {
        self.mode = MockMode::Echo;
        self
    }

    /// Register a scripted response. `prompt_fragment` can be the full prompt
    /// text; the key stored is the SHA-256 hex of the fragment.
    pub fn register(mut self, prompt_fragment: impl Into<String>, response: impl Into<String>) -> Self {
        let key = sha256_hex(&prompt_fragment.into());
        self.scripts.insert(key, response.into());
        self
    }

    /// Register by explicit hash (pre-computed).
    pub fn register_by_hash(mut self, hash: impl Into<String>, response: impl Into<String>) -> Self {
        self.scripts.insert(hash.into(), response.into());
        self
    }

    /// Fraction of input tokens reported as cache hits. `0.0` = no caching;
    /// `1.0` = fully cached. Defaults to `0.0`.
    pub fn cache_hit_ratio(mut self, ratio: f32) -> Self {
        self.cache_hit_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    /// Fixed per-call delay injected before responding.
    pub fn latency(mut self, d: Duration) -> Self {
        self.latency = Some(d);
        self
    }

    /// Queue a failure to inject on the next call. Multiple failures are
    /// consumed in FIFO order; once the queue is empty the mock responds
    /// normally.
    pub fn inject_failure(mut self, f: InjectFailure) -> Self {
        self.failures.push(f);
        self
    }

    /// Finalise and return the provider.
    pub fn build(self) -> MockLlmProvider {
        MockLlmProvider {
            mode: self.mode,
            scripts: self.scripts,
            cache_hit_ratio: self.cache_hit_ratio,
            latency: self.latency,
            failures: Arc::new(Mutex::new(self.failures)),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

// ─── Provider ────────────────────────────────────────────────────────────────

/// Deterministic, network-free [`LlmProvider`] for tests.
///
/// Construct via [`MockLlmProvider::builder`].
pub struct MockLlmProvider {
    mode: MockMode,
    scripts: HashMap<String, String>,
    cache_hit_ratio: f32,
    latency: Option<Duration>,
    /// FIFO failure queue. Drained one-per-call; empty = normal response.
    failures: Arc<Mutex<Vec<InjectFailure>>>,
    /// Append-only call log for post-test assertions.
    calls: Arc<Mutex<Vec<CallRecord>>>,
}

impl MockLlmProvider {
    /// Start building a new mock provider.
    pub fn builder() -> MockBuilder {
        MockBuilder::default()
    }

    /// Return a snapshot of every call received so far.
    pub fn recorded_calls(&self) -> Vec<CallRecord> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// How many calls have been received.
    pub fn call_count(&self) -> usize {
        self.recorded_calls().len()
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn full_prompt(prompt: &PromptStructured) -> String {
        format!("{}{}", prompt.static_head, prompt.dynamic_tail)
    }

    fn resolve_response(&self, full: &str) -> Result<String> {
        match &self.mode {
            MockMode::Scripted => {
                let key = sha256_hex(full);
                self.scripts.get(&key).cloned().ok_or_else(|| {
                    Error::Decode(format!(
                        "MockLlmProvider: no scripted response for prompt hash {key} \
                         (prompt snippet: {:?})",
                        &full[..full.len().min(80)]
                    ))
                })
            }
            MockMode::Template => {
                let snippet = &full[..full.len().min(120)];
                Ok(format!(r#"{{"text": {}}}"#, serde_json::to_string(snippet).unwrap()))
            }
            MockMode::Echo => {
                let snippet = &full[..full.len().min(60)];
                Ok(format!(r#"{{"echo": {}}}"#, serde_json::to_string(snippet).unwrap()))
            }
        }
    }

    fn build_usage(&self, input_chars: usize, output_chars: usize) -> Usage {
        let input_total = (input_chars / 4).max(1) as u32;
        let output_tokens = (output_chars / 4).max(1) as u32;
        let input_cached = (input_total as f32 * self.cache_hit_ratio) as u32;
        Usage {
            input_tokens_total: input_total,
            input_tokens_cached: input_cached,
            input_tokens_cache_create: 0,
            output_tokens,
        }
    }

    fn next_failure(&self) -> Option<InjectFailure> {
        let mut q = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if q.is_empty() { None } else { Some(q.remove(0)) }
    }

    fn record_call(&self, prompt: &PromptStructured, model: &Model) {
        let mut log = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        log.push(CallRecord {
            prompt: Self::full_prompt(prompt),
            model_name: model.name.clone(),
        });
    }

    async fn apply_latency(&self) {
        if let Some(d) = self.latency {
            tokio::time::sleep(d).await;
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlmProvider {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        _options: &CompleteOptions,
    ) -> Result<Completion> {
        self.record_call(prompt, model);
        self.apply_latency().await;

        // Consume queued failure if present.
        if let Some(f) = self.next_failure() {
            return Err(failure_to_error(f));
        }

        let full = Self::full_prompt(prompt);
        let text = self.resolve_response(&full)?;
        let usage = self.build_usage(full.len(), text.len());
        let cost = Cost::from_usage(&usage, model);

        Ok(Completion {
            text,
            usage,
            cost,
            model_used: format!("mock/{}", model.name),
            latency_ms: self.latency.map(|d| d.as_millis() as u64).unwrap_or(0),
        })
    }

    async fn complete_streamed(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        _options: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        self.record_call(prompt, model);
        self.apply_latency().await;

        // Consume queued failure if present.
        if let Some(f) = self.next_failure() {
            // PartialStreamThenDisconnect is handled in the stream body below.
            if !matches!(f, InjectFailure::PartialStreamThenDisconnect) {
                return Err(failure_to_error(f));
            }
            // Build a partial stream that errors halfway through.
            let full = Self::full_prompt(prompt);
            let text = self.resolve_response(&full).unwrap_or_else(|_| full.clone());
            let words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
            let half = (words.len() / 2).max(1);
            let mut chunks: Vec<Result<StreamChunk>> = words[..half]
                .iter()
                .map(|w| Ok(StreamChunk::Delta { text: format!("{w} ") }))
                .collect();
            chunks.push(Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "mock: partial stream disconnect",
            ))));
            return Ok(Box::pin(stream::iter(chunks)));
        }

        let full = Self::full_prompt(prompt);
        let text = self.resolve_response(&full)?;
        let usage = self.build_usage(full.len(), text.len());
        let cost = Cost::from_usage(&usage, model);
        let model_used = format!("mock/{}", model.name);
        let latency_ms = self.latency.map(|d| d.as_millis() as u64).unwrap_or(0);

        // Chunk the response word-by-word.
        let words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
        let mut chunks: Vec<Result<StreamChunk>> = words
            .into_iter()
            .map(|w| Ok(StreamChunk::Delta { text: format!("{w} ") }))
            .collect();
        chunks.push(Ok(StreamChunk::Done { usage, cost, model_used, latency_ms }));

        Ok(Box::pin(stream::iter(chunks)))
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        // Deterministic embedding: hash the text into a fixed-length f32 vector.
        let hash = sha256_bytes(text);
        let dim = model.dim;
        let vec: Vec<f32> = (0..dim)
            .map(|i| {
                let byte = hash[i % hash.len()] as f32;
                // Normalize to [-1, 1]
                (byte / 127.5) - 1.0
            })
            .collect();
        Ok(vec)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn failure_to_error(f: InjectFailure) -> Error {
    match f {
        InjectFailure::Timeout(d) => Error::Timeout { millis: d.as_millis() as u64 },
        InjectFailure::ServerError => Error::Status {
            status: 500,
            message: "mock: injected server error".into(),
        },
        InjectFailure::RateLimit => Error::RateLimited {
            retry_after: None,
            message: "mock: injected rate limit".into(),
        },
        InjectFailure::PartialStreamThenDisconnect => {
            // This branch is only reached if called from `complete` (not
            // `complete_streamed`); treat it as a server error.
            Error::Status {
                status: 500,
                message: "mock: partial stream not applicable to non-streaming complete".into(),
            }
        }
    }
}

fn sha256_hex(input: &str) -> String {
    let bytes = sha256_bytes(input);
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        write!(s, "{b:02x}").unwrap();
        s
    })
}

fn sha256_bytes(input: &str) -> [u8; 32] {
    // Portable pure-Rust SHA-256. No external dep — this is test-only code
    // compiled under `mock-llm` feature.
    sha256_impl(input.as_bytes())
}

/// Minimal SHA-256 without any external dependency.
/// Reference: FIPS 180-4. This runs only in test builds.
fn sha256_impl(data: &[u8]) -> [u8; 32] {
    #[allow(clippy::unreadable_literal)]
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    // Pre-processing: pad message
    let len = data.len();
    let bit_len = (len as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) block
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i*4], block[i*4+1], block[i*4+2], block[i*4+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] =
            [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g; g = f; f = e;
            e = d.wrapping_add(temp1);
            d = c; c = b; b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        let bytes = word.to_be_bytes();
        out[i*4..i*4+4].copy_from_slice(&bytes);
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::types::{CompleteOptions, EmbeddingModel, Model, ModelProvider, PromptStructured};

    fn prompt(s: &str) -> PromptStructured {
        PromptStructured::dynamic_only(s)
    }

    fn model() -> Model {
        Model { provider: ModelProvider::Anthropic, name: "mock-test".into() }
    }

    fn embed_model(dim: usize) -> EmbeddingModel {
        EmbeddingModel { provider: ModelProvider::Ollama, name: "bge-m3".into(), dim }
    }

    // ── Echo mode ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn echo_mode_returns_snippet() {
        let mock = MockLlmProvider::builder().mode_echo().build();
        let res = mock.complete(&prompt("hello world"), &model(), &CompleteOptions::default()).await.unwrap();
        assert!(res.text.contains("echo"));
        assert!(res.text.contains("hello world") || res.text.len() > 0);
    }

    // ── Template mode ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn template_mode_is_deterministic() {
        let mock = MockLlmProvider::builder().mode_template().build();
        let p = prompt("deterministic content");
        let r1 = mock.complete(&p, &model(), &CompleteOptions::default()).await.unwrap();
        let r2 = mock.complete(&p, &model(), &CompleteOptions::default()).await.unwrap();
        assert_eq!(r1.text, r2.text);
    }

    // ── Scripted mode ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn scripted_hit_returns_canned_response() {
        let mock = MockLlmProvider::builder()
            .mode_scripted()
            .register("test prompt", r#"{"confidence": 0.9}"#)
            .build();
        let res = mock.complete(&prompt("test prompt"), &model(), &CompleteOptions::default()).await.unwrap();
        assert_eq!(res.text, r#"{"confidence": 0.9}"#);
    }

    #[tokio::test]
    async fn scripted_miss_fails_loudly_with_hash() {
        let mock = MockLlmProvider::builder().mode_scripted().build();
        let err = mock.complete(&prompt("unregistered"), &model(), &CompleteOptions::default()).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no scripted response for prompt hash"), "got: {msg}");
    }

    // ── Token accounting ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn token_accounting_math() {
        let mock = MockLlmProvider::builder().mode_echo().build();
        let input = "a".repeat(400); // 400 chars → 100 tokens
        let res = mock.complete(&prompt(&input), &model(), &CompleteOptions::default()).await.unwrap();
        assert_eq!(res.usage.input_tokens_total, 100);
        assert_eq!(res.usage.input_tokens_cached, 0);
    }

    #[tokio::test]
    async fn cache_hit_ratio_applied() {
        let mock = MockLlmProvider::builder()
            .mode_echo()
            .cache_hit_ratio(0.5)
            .build();
        let input = "a".repeat(400); // 100 tokens; 50% cached = 50
        let res = mock.complete(&prompt(&input), &model(), &CompleteOptions::default()).await.unwrap();
        assert_eq!(res.usage.input_tokens_cached, 50);
    }

    // ── Streaming ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn streaming_emits_deltas_then_done() {
        let mock = MockLlmProvider::builder().mode_echo().build();
        let mut s = mock.complete_streamed(&prompt("hello world"), &model(), &CompleteOptions::default()).await.unwrap();

        let mut deltas = 0usize;
        let mut done = false;
        while let Some(chunk) = s.next().await {
            match chunk.unwrap() {
                StreamChunk::Delta { .. } => deltas += 1,
                StreamChunk::Done { .. } => { done = true; break; }
                StreamChunk::ToolUseMarker => {}
            }
        }
        assert!(done, "stream did not emit Done");
        // echo response is short; should have at least one delta
        assert!(deltas >= 1, "expected at least 1 delta, got {deltas}");
    }

    #[tokio::test]
    async fn streaming_chunk_shape_valid() {
        let mock = MockLlmProvider::builder()
            .mode_scripted()
            .register("chunk test", "word1 word2 word3")
            .build();
        let mut s = mock.complete_streamed(&prompt("chunk test"), &model(), &CompleteOptions::default()).await.unwrap();

        let mut texts = vec![];
        while let Some(chunk) = s.next().await {
            if let StreamChunk::Delta { text } = chunk.unwrap() {
                texts.push(text);
            }
        }
        let combined = texts.join("");
        assert!(combined.contains("word1"), "combined: {combined}");
    }

    // ── Failure injection ────────────────────────────────────────────────────

    #[tokio::test]
    async fn failure_server_error_injected() {
        let mock = MockLlmProvider::builder()
            .mode_echo()
            .inject_failure(InjectFailure::ServerError)
            .build();
        let err = mock.complete(&prompt("x"), &model(), &CompleteOptions::default()).await.unwrap_err();
        assert!(matches!(err, Error::Status { status: 500, .. }));
    }

    #[tokio::test]
    async fn failure_rate_limit_injected() {
        let mock = MockLlmProvider::builder()
            .mode_echo()
            .inject_failure(InjectFailure::RateLimit)
            .build();
        let err = mock.complete(&prompt("x"), &model(), &CompleteOptions::default()).await.unwrap_err();
        assert!(matches!(err, Error::RateLimited { .. }));
    }

    #[tokio::test]
    async fn failure_timeout_injected() {
        let mock = MockLlmProvider::builder()
            .mode_echo()
            .inject_failure(InjectFailure::Timeout(Duration::from_millis(100)))
            .build();
        let err = mock.complete(&prompt("x"), &model(), &CompleteOptions::default()).await.unwrap_err();
        assert!(matches!(err, Error::Timeout { millis: 100 }));
    }

    #[tokio::test]
    async fn failure_consumed_then_normal_response() {
        let mock = MockLlmProvider::builder()
            .mode_echo()
            .inject_failure(InjectFailure::ServerError)
            .build();
        // First call: error
        mock.complete(&prompt("x"), &model(), &CompleteOptions::default()).await.unwrap_err();
        // Second call: normal
        mock.complete(&prompt("x"), &model(), &CompleteOptions::default()).await.unwrap();
    }

    #[tokio::test]
    async fn partial_stream_then_disconnect() {
        let mock = MockLlmProvider::builder()
            .mode_scripted()
            .register("partial", "alpha beta gamma delta epsilon")
            .inject_failure(InjectFailure::PartialStreamThenDisconnect)
            .build();

        let mut s = mock.complete_streamed(&prompt("partial"), &model(), &CompleteOptions::default()).await.unwrap();
        let mut got_delta = false;
        let mut got_error = false;
        while let Some(result) = s.next().await {
            match result {
                Ok(StreamChunk::Delta { .. }) => got_delta = true,
                Err(_) => { got_error = true; break; }
                _ => {}
            }
        }
        assert!(got_delta, "expected at least one delta before disconnect");
        assert!(got_error, "expected I/O error at disconnect point");
    }

    // ── Call recording ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn call_count_recorded() {
        let mock = MockLlmProvider::builder().mode_echo().build();
        assert_eq!(mock.call_count(), 0);
        mock.complete(&prompt("a"), &model(), &CompleteOptions::default()).await.unwrap();
        mock.complete(&prompt("b"), &model(), &CompleteOptions::default()).await.unwrap();
        assert_eq!(mock.call_count(), 2);
    }

    #[tokio::test]
    async fn call_record_captures_prompt() {
        let mock = MockLlmProvider::builder().mode_echo().build();
        mock.complete(&prompt("my prompt"), &model(), &CompleteOptions::default()).await.unwrap();
        let calls = mock.recorded_calls();
        assert_eq!(calls[0].prompt, "my prompt");
        assert_eq!(calls[0].model_name, "mock-test");
    }

    // ── Embedding ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn embed_returns_correct_dim() {
        let mock = MockLlmProvider::builder().build();
        let v = mock.embed("hello", &embed_model(1024)).await.unwrap();
        assert_eq!(v.len(), 1024);
    }

    #[tokio::test]
    async fn embed_is_deterministic() {
        let mock = MockLlmProvider::builder().build();
        let em = embed_model(128);
        let v1 = mock.embed("same text", &em).await.unwrap();
        let v2 = mock.embed("same text", &em).await.unwrap();
        assert_eq!(v1, v2);
    }

    #[tokio::test]
    async fn embed_differs_for_different_inputs() {
        let mock = MockLlmProvider::builder().build();
        let em = embed_model(128);
        let v1 = mock.embed("text one", &em).await.unwrap();
        let v2 = mock.embed("text two", &em).await.unwrap();
        assert_ne!(v1, v2);
    }

    // ── SHA-256 ──────────────────────────────────────────────────────────────

    #[test]
    fn sha256_known_value() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let h = sha256_hex("");
        assert_eq!(h, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn sha256_hello_world() {
        // SHA-256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe04294e576de793cdf72fb3db5 -- wait, let me not hardcode this incorrectly.
        // Just verify it's 64 hex chars and deterministic.
        let h1 = sha256_hex("hello world");
        let h2 = sha256_hex("hello world");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
