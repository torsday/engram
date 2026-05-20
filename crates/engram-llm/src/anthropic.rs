//! Anthropic provider — Claude Messages API with prompt caching.
//!
//! Wire format implemented:
//!
//! - **Request**: POST `/v1/messages`, headers `x-api-key`, `anthropic-version`,
//!   `content-type: application/json`. The structured prompt is split into two
//!   user-message content blocks; the first carries
//!   `cache_control: { type: "ephemeral" }`.
//!
//! - **Response**: assembled by concatenating the `text` field of every
//!   `content` block of type `"text"`. Tool-use blocks (when added in a later
//!   slice) will be surfaced separately.
//!
//! - **Usage extraction**: `usage.input_tokens` and
//!   `usage.cache_read_input_tokens` + `usage.cache_creation_input_tokens` are
//!   read into [`crate::Usage`]. Anthropic reports the cached share separately
//!   from the total, so `input_tokens_total` is the sum of all three buckets.
//!
//! ## Embeddings
//!
//! Anthropic does not host an embeddings endpoint. [`AnthropicProvider::embed`]
//! returns [`crate::Error::UnsupportedByProvider`]. Use the OpenAI or Voyage
//! provider for embeddings.
//!
//! ## Streaming
//!
//! Not implemented in this slice — see crate-level scope note. The trait
//! does not yet expose a `complete_streamed` method; that arrives with the
//! streaming follow-up.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use reqwest::Client;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::types::{
    CompleteOptions, Completion, EmbeddingModel, Model, ModelProvider, PromptStructured, Usage,
};

/// Anthropic API version pinned in the `anthropic-version` header.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default base URL — tests override this to point at `wiremock`.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic [`LlmProvider`] implementation.
pub struct AnthropicProvider {
    http: Client,
    base_url: String,
    secrets: Arc<dyn engram_secrets::SecretsStore>,
}

impl AnthropicProvider {
    /// Build a provider with a default `reqwest` client and the given base
    /// URL. Pass [`DEFAULT_BASE_URL`] in production; tests pass the mock
    /// server's URL.
    pub fn new(
        secrets: Arc<dyn engram_secrets::SecretsStore>,
        base_url: impl Into<String>,
    ) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("engram-llm/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            secrets,
        })
    }

    /// Default base URL (`https://api.anthropic.com`).
    pub const DEFAULT_BASE_URL: &'static str = DEFAULT_BASE_URL;

    fn require_anthropic(&self, model: &Model, op: &'static str) -> Result<()> {
        if model.provider != ModelProvider::Anthropic {
            return Err(Error::UnsupportedByProvider {
                provider: "anthropic",
                op,
            });
        }
        Ok(())
    }

    async fn api_key(&self) -> Result<String> {
        let secret = self.secrets.get("anthropic")?;
        Ok(secret.expose_secret().to_string())
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        self.require_anthropic(model, "complete")?;
        let api_key = self.api_key().await?;
        let body = build_messages_body(prompt, model, options);

        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;
        let latency_ms = started.elapsed().as_millis() as u64;

        if !status.is_success() {
            let message = parse_error_message(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: MessagesResponse = serde_json::from_str(&body_text)
            .map_err(|e| Error::Decode(format!("messages response: {e}")))?;

        let text = parsed
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("");

        if text.is_empty() {
            return Err(Error::EmptyResponse);
        }

        let usage = Usage {
            input_tokens_total: parsed.usage.input_tokens
                + parsed.usage.cache_read_input_tokens.unwrap_or(0)
                + parsed.usage.cache_creation_input_tokens.unwrap_or(0),
            input_tokens_cached: parsed.usage.cache_read_input_tokens.unwrap_or(0),
            input_tokens_cache_create: parsed.usage.cache_creation_input_tokens.unwrap_or(0),
            output_tokens: parsed.usage.output_tokens,
        };

        let model_used = format!("anthropic/{}", parsed.model);

        tracing::info!(
            target: "engram_llm::anthropic",
            model = %model_used,
            input_tokens_total = usage.input_tokens_total,
            input_tokens_cached = usage.input_tokens_cached,
            input_tokens_cache_create = usage.input_tokens_cache_create,
            output_tokens = usage.output_tokens,
            cache_hit_ratio = usage.cache_hit_ratio(),
            latency_ms,
            "anthropic.complete ok"
        );

        Ok(Completion {
            text,
            usage,
            model_used,
            latency_ms,
        })
    }

    async fn embed(&self, _text: &str, _model: &EmbeddingModel) -> Result<Vec<f32>> {
        Err(Error::UnsupportedByProvider {
            provider: "anthropic",
            op: "embed",
        })
    }
}

/// Build the JSON request body for `POST /v1/messages`.
///
/// Behavior:
/// - If `static_head` is non-empty, the user message has two content blocks;
///   the first carries `cache_control: { "type": "ephemeral" }`.
/// - If `static_head` is empty, the user message has one content block with
///   no cache marker (sending an empty cached prefix wastes a request).
/// - `system`, `temperature`, `max_tokens`, `stop_sequences` are mapped to
///   their Anthropic names where they differ.
///
/// Exposed at module scope (not `pub`) so the unit tests can exercise the
/// wire format without spinning up a server.
fn build_messages_body(
    prompt: &PromptStructured,
    model: &Model,
    options: &CompleteOptions,
) -> serde_json::Value {
    use serde_json::json;

    let content: Vec<serde_json::Value> = if prompt.static_head.is_empty() {
        vec![json!({ "type": "text", "text": prompt.dynamic_tail })]
    } else {
        vec![
            json!({
                "type": "text",
                "text": prompt.static_head,
                "cache_control": { "type": "ephemeral" }
            }),
            json!({ "type": "text", "text": prompt.dynamic_tail }),
        ]
    };

    let mut body = json!({
        "model": model.name,
        "max_tokens": options.max_tokens,
        "temperature": options.temperature,
        "messages": [ { "role": "user", "content": content } ],
    });

    if let Some(sys) = &options.system {
        body["system"] = json!(sys);
    }
    if !options.stop_sequences.is_empty() {
        body["stop_sequences"] = json!(options.stop_sequences);
    }

    body
}

/// Best-effort extraction of `error.message` from an Anthropic error body.
/// Returns `None` if the body isn't shaped that way — the caller falls back
/// to the HTTP status code.
fn parse_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_string)
}

// ─── wire-shape deserialization ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    model: String,
    content: Vec<ContentBlock>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize, Serialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_body_emits_cache_marker_when_head_present() {
        let prompt = PromptStructured::new("static", "dynamic");
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let body = build_messages_body(&prompt, &model, &CompleteOptions::default());

        let content = &body["messages"][0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 2);
        assert_eq!(content[0]["text"], "static");
        assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(content[1]["text"], "dynamic");
        // Tail must NOT have a cache marker.
        assert!(content[1].get("cache_control").is_none());
    }

    #[test]
    fn build_body_omits_cache_marker_when_head_empty() {
        let prompt = PromptStructured::dynamic_only("just dynamic");
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let body = build_messages_body(&prompt, &model, &CompleteOptions::default());

        let content = &body["messages"][0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert!(content[0].get("cache_control").is_none());
    }

    #[test]
    fn build_body_passes_through_options() {
        let prompt = PromptStructured::dynamic_only("x");
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let opts = CompleteOptions {
            temperature: 0.7,
            max_tokens: 256,
            stop_sequences: vec!["STOP".into()],
            system: Some("You are testy.".into()),
        };
        let body = build_messages_body(&prompt, &model, &opts);

        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < 1e-5, "expected ~0.7, got {temp}");
        assert_eq!(body["max_tokens"], 256);
        assert_eq!(body["stop_sequences"][0], "STOP");
        assert_eq!(body["system"], "You are testy.");
    }

    #[test]
    fn parse_error_message_handles_typical_shape() {
        let body =
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad prompt"}}"#;
        assert_eq!(parse_error_message(body).as_deref(), Some("bad prompt"));
    }

    #[test]
    fn parse_error_message_returns_none_for_unparseable() {
        assert!(parse_error_message("not json").is_none());
        assert!(parse_error_message("{}").is_none());
    }

    #[test]
    fn messages_response_extracts_text_and_usage() {
        let payload = r#"{
            "id": "msg_1",
            "model": "claude-3-5-haiku-20241022",
            "role": "assistant",
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": " world" }
            ],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 5,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 0
            }
        }"#;
        let parsed: MessagesResponse = serde_json::from_str(payload).unwrap();
        let text: String = parsed
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "hello world");
        assert_eq!(parsed.usage.input_tokens, 100);
        assert_eq!(parsed.usage.cache_read_input_tokens, Some(80));
    }

    #[test]
    fn messages_response_tolerates_unknown_block_types() {
        // Tool-use blocks will arrive in a later slice; current code must
        // skip rather than fail.
        let payload = r#"{
            "id": "msg_x",
            "model": "claude-3-5-sonnet-20241022",
            "role": "assistant",
            "content": [
                { "type": "tool_use", "id": "x", "name": "y", "input": {} },
                { "type": "text", "text": "answer" }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }"#;
        let parsed: MessagesResponse = serde_json::from_str(payload).unwrap();
        let text: String = parsed
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "answer");
    }
}
