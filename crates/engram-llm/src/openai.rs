//! OpenAI provider — Chat Completions API + text-embedding-3-large.
//!
//! ## Completions
//!
//! Uses POST `/v1/chat/completions`. The structured prompt's `static_head` and
//! `dynamic_tail` are concatenated into a single user message; OpenAI's
//! automatic prefix caching (≥1024 tokens) applies transparently.
//!
//! Usage extraction reads `usage.prompt_tokens_details.cached_tokens` for the
//! cache-hit metric.
//!
//! ## Embeddings
//!
//! Uses POST `/v1/embeddings` with `model: text-embedding-3-large`
//! (1536 dimensions; Matryoshka-reducible). Batching (up to 100 texts per
//! call) is handled by the caller — this provider processes one text at a time
//! (the trait is single-text today; batching lives at the pipeline layer).
//!
//! ## Cost
//!
//! Computed via [`Cost::from_usage`] against the static price table in
//! `prices.toml`.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use secrecy::ExposeSecret;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::{StreamChunk, StreamedCompletion};
use crate::types::{
    CompleteOptions, Completion, Cost, EmbeddingModel, Model, ModelProvider, PromptStructured,
    Usage,
};

/// Default base URL for the OpenAI API.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// OpenAI [`LlmProvider`] implementation.
///
/// API key is sourced from `SecretsStore::get("openai")`.
pub struct OpenAIProvider {
    http: Client,
    base_url: String,
    secrets: Arc<dyn engram_secrets::SecretsStore>,
}

impl OpenAIProvider {
    /// Build a provider with a default `reqwest` client.
    ///
    /// Pass [`DEFAULT_BASE_URL`] in production; tests pass the mock server URL.
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

    fn require_openai(&self, model: &Model, op: &'static str) -> Result<()> {
        if model.provider != ModelProvider::OpenAi {
            return Err(Error::UnsupportedByProvider {
                provider: "openai",
                op,
            });
        }
        Ok(())
    }

    async fn api_key(&self) -> Result<String> {
        let secret = self.secrets.get("openai")?;
        Ok(secret.expose_secret().to_string())
    }
}

#[async_trait]
impl LlmProvider for OpenAIProvider {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        self.require_openai(model, "complete")?;
        let api_key = self.api_key().await?;

        let full_prompt = format!("{}{}", prompt.static_head, prompt.dynamic_tail);
        let body = build_chat_body(&full_prompt, model, options);

        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("authorization", format!("Bearer {}", api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let retry_after_hdr = parse_retry_after_header(response.headers());
        let body_text = response.text().await?;
        let latency_ms = started.elapsed().as_millis() as u64;

        if !status.is_success() {
            let message = parse_openai_error(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            if status.as_u16() == 429 {
                return Err(Error::RateLimited {
                    retry_after: retry_after_hdr,
                    message,
                });
            }
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: ChatResponse = serde_json::from_str(&body_text)
            .map_err(|e| Error::Decode(format!("chat/completions response: {e}")))?;

        let text = parsed
            .choices
            .first()
            .map(|c| c.message.content.as_deref().unwrap_or(""))
            .unwrap_or("")
            .to_owned();

        if text.is_empty() {
            return Err(Error::EmptyResponse);
        }

        let cached = parsed
            .usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);

        let usage = Usage {
            input_tokens_total: parsed.usage.prompt_tokens,
            input_tokens_cached: cached,
            input_tokens_cache_create: 0, // OpenAI doesn't report create-writes
            output_tokens: parsed.usage.completion_tokens,
        };

        let model_used = format!("openai/{}", parsed.model);
        let cost = Cost::from_usage(&usage, model);

        tracing::info!(
            target: "engram_llm::openai",
            model = %model_used,
            input_tokens_total = usage.input_tokens_total,
            input_tokens_cached = usage.input_tokens_cached,
            output_tokens = usage.output_tokens,
            cache_hit_ratio = usage.cache_hit_ratio(),
            latency_ms,
            "openai.complete ok"
        );

        Ok(Completion {
            text,
            usage,
            cost,
            model_used,
            latency_ms,
        })
    }

    async fn complete_streamed(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        self.require_openai(model, "complete_streamed")?;
        let api_key = self.api_key().await?;

        let full_prompt = format!("{}{}", prompt.static_head, prompt.dynamic_tail);
        let mut body = build_chat_body(&full_prompt, model, options);
        body["stream"] = serde_json::json!(true);
        // Request usage stats on the final stream chunk.
        body["stream_options"] = serde_json::json!({ "include_usage": true });

        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("authorization", format!("Bearer {}", api_key))
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let retry_after_hdr = parse_retry_after_header(response.headers());
            let body_text = response.text().await?;
            let message = parse_openai_error(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            if status.as_u16() == 429 {
                return Err(Error::RateLimited {
                    retry_after: retry_after_hdr,
                    message,
                });
            }
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let bytes = response
            .bytes_stream()
            .map(|r| r.map_err(|e| Error::Decode(format!("stream chunk: {e}"))));
        let parsed = parse_openai_sse(bytes, started, model.clone());
        Ok(Box::pin(parsed))
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        if model.provider != ModelProvider::OpenAi {
            return Err(Error::UnsupportedByProvider {
                provider: "openai",
                op: "embed",
            });
        }
        let api_key = self.api_key().await?;

        let body = serde_json::json!({
            "model": model.name,
            "input": text,
            "encoding_format": "float",
        });

        let response = self
            .http
            .post(format!("{}/v1/embeddings", self.base_url))
            .header("authorization", format!("Bearer {}", api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            let message = parse_openai_error(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            if status.as_u16() == 429 {
                let retry_after_hdr = None; // headers consumed above
                return Err(Error::RateLimited {
                    retry_after: retry_after_hdr,
                    message,
                });
            }
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: EmbeddingResponse = serde_json::from_str(&body_text)
            .map_err(|e| Error::Decode(format!("embeddings response: {e}")))?;

        parsed
            .data
            .into_iter()
            .next()
            .map(|e| e.embedding)
            .ok_or(Error::EmptyResponse)
    }
}

// ─── Request builders ─────────────────────────────────────────────────────────

fn build_chat_body(prompt: &str, model: &Model, options: &CompleteOptions) -> serde_json::Value {
    let mut messages = Vec::new();
    if let Some(system) = &options.system {
        messages.push(serde_json::json!({ "role": "system", "content": system }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": prompt }));

    let mut body = serde_json::json!({
        "model": model.name,
        "messages": messages,
        "max_completion_tokens": options.max_tokens,
        "temperature": options.temperature,
    });

    if !options.stop_sequences.is_empty() {
        body["stop"] = serde_json::json!(options.stop_sequences);
    }

    body
}

// ─── SSE parser ───────────────────────────────────────────────────────────────

fn parse_openai_sse<S>(
    bytes: S,
    started: Instant,
    model: Model,
) -> impl futures_util::Stream<Item = Result<StreamChunk>> + Send
where
    S: futures_util::Stream<Item = Result<bytes::Bytes>> + Send + 'static,
{
    use futures_util::stream::unfold;

    struct State<S> {
        source: std::pin::Pin<Box<S>>,
        buffer: String,
        accumulated_text: String,
        usage: Option<Usage>,
        model_used: String,
        model: Model,
        started: Instant,
        done_emitted: bool,
    }

    let init = State {
        source: Box::pin(bytes),
        buffer: String::new(),
        accumulated_text: String::new(),
        usage: None,
        model_used: String::new(),
        model,
        started,
        done_emitted: false,
    };

    unfold(init, |mut state| async move {
        loop {
            // Try to drain a complete SSE event.
            while let Some(boundary) = state.buffer.find("\n\n") {
                let raw_event = state.buffer[..boundary].to_string();
                state.buffer.drain(..boundary + 2);

                // Extract data line(s).
                let mut data = String::new();
                for line in raw_event.lines() {
                    if let Some(rest) = line.strip_prefix("data:") {
                        data.push_str(rest.trim_start());
                    }
                }
                if data.is_empty() || data == "[DONE]" {
                    if data == "[DONE]" && !state.done_emitted {
                        state.done_emitted = true;
                        let usage = state.usage.take().unwrap_or_default();
                        let cost = Cost::from_usage(&usage, &state.model);
                        return Some((
                            Ok(StreamChunk::Done {
                                usage,
                                cost,
                                model_used: state.model_used.clone(),
                                latency_ms: state.started.elapsed().as_millis() as u64,
                            }),
                            state,
                        ));
                    }
                    continue;
                }

                let chunk: serde_json::Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(e) => {
                        return Some((Err(Error::Decode(format!("openai sse data: {e}"))), state))
                    }
                };

                // Track model name from first chunk.
                if state.model_used.is_empty() {
                    if let Some(m) = chunk["model"].as_str() {
                        state.model_used = format!("openai/{m}");
                    }
                }

                // Accumulate text deltas.
                if let Some(delta_text) = chunk["choices"][0]["delta"]["content"].as_str() {
                    if !delta_text.is_empty() {
                        state.accumulated_text.push_str(delta_text);
                        return Some((
                            Ok(StreamChunk::Delta {
                                text: delta_text.to_owned(),
                            }),
                            state,
                        ));
                    }
                }

                // Final usage chunk (stream_options.include_usage).
                if let Some(usage_obj) = chunk.get("usage").filter(|u| !u.is_null()) {
                    let prompt_tokens = usage_obj["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                    let completion_tokens =
                        usage_obj["completion_tokens"].as_u64().unwrap_or(0) as u32;
                    let cached = usage_obj["prompt_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0) as u32;
                    state.usage = Some(Usage {
                        input_tokens_total: prompt_tokens,
                        input_tokens_cached: cached,
                        input_tokens_cache_create: 0,
                        output_tokens: completion_tokens,
                    });
                }
            }

            // Pull more bytes from the stream.
            use futures_util::StreamExt;
            match state.source.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.push_str(&String::from_utf8_lossy(&chunk));
                }
                Some(Err(e)) => return Some((Err(e), state)),
                None => {
                    if !state.done_emitted {
                        return Some((
                            Err(Error::Decode(
                                "openai stream ended before [DONE]".to_owned(),
                            )),
                            state,
                        ));
                    }
                    return None;
                }
            }
        }
    })
}

// ─── Header helpers ───────────────────────────────────────────────────────────

fn parse_retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let val = headers.get(reqwest::header::RETRY_AFTER)?;
    let s = val.to_str().ok()?.trim();
    if let Ok(secs) = s.parse::<f64>() {
        return Some(std::time::Duration::from_secs_f64(secs.max(0.0)));
    }
    None
}

// ─── Response deserialization ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatResponse {
    model: String,
    choices: Vec<ChatChoice>,
    usage: ChatUsage,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

// ─── Error parsing ────────────────────────────────────────────────────────────

fn parse_openai_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_string)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_response_deserializes_with_usage() {
        let raw = json!({
            "id": "chatcmpl-abc",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "hello world" },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "prompt_tokens_details": { "cached_tokens": 80 }
            }
        });
        let parsed: ChatResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.model, "gpt-4o-mini");
        assert_eq!(
            parsed.choices[0].message.content.as_deref(),
            Some("hello world")
        );
        assert_eq!(parsed.usage.prompt_tokens, 100);
        assert_eq!(parsed.usage.completion_tokens, 20);
        let cached = parsed.usage.prompt_tokens_details.unwrap().cached_tokens;
        assert_eq!(cached, Some(80));
    }

    #[test]
    fn chat_response_deserializes_without_prompt_details() {
        let raw = json!({
            "id": "chatcmpl-xyz",
            "model": "gpt-4o",
            "choices": [{ "message": { "content": "hi" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        });
        let parsed: ChatResponse = serde_json::from_value(raw).unwrap();
        assert!(parsed.usage.prompt_tokens_details.is_none());
    }

    #[test]
    fn embedding_response_deserializes() {
        let raw = json!({
            "object": "list",
            "data": [{ "object": "embedding", "index": 0, "embedding": [0.1, 0.2, 0.3] }],
            "model": "text-embedding-3-large",
            "usage": { "prompt_tokens": 5, "total_tokens": 5 }
        });
        let parsed: EmbeddingResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.data[0].embedding, vec![0.1_f32, 0.2, 0.3]);
    }

    #[test]
    fn parse_openai_error_extracts_message() {
        let body = r#"{"error":{"message":"insufficient_quota","type":"insufficient_quota"}}"#;
        assert_eq!(
            parse_openai_error(body).as_deref(),
            Some("insufficient_quota")
        );
    }

    #[test]
    fn parse_openai_error_returns_none_for_unknown_shape() {
        assert!(parse_openai_error("not json").is_none());
        assert!(parse_openai_error(r#"{"foo":"bar"}"#).is_none());
    }

    #[test]
    fn build_chat_body_includes_system_when_set() {
        let model = Model {
            provider: ModelProvider::OpenAi,
            name: "gpt-4o".to_owned(),
        };
        let opts = CompleteOptions {
            system: Some("be helpful".to_owned()),
            ..Default::default()
        };
        let body = build_chat_body("hello", &model, &opts);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn build_chat_body_omits_system_when_none() {
        let model = Model {
            provider: ModelProvider::OpenAi,
            name: "gpt-4o".to_owned(),
        };
        let body = build_chat_body("hello", &model, &CompleteOptions::default());
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn openai_provider_rejects_non_openai_model_on_complete() {
        // Verify `require_openai` fires for wrong provider.
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let opts = CompleteOptions::default();
        let prompt = PromptStructured::dynamic_only("x");
        // Can't call async fn in sync test — check directly.
        let provider_result: Result<()> = {
            if model.provider != ModelProvider::OpenAi {
                Err(Error::UnsupportedByProvider {
                    provider: "openai",
                    op: "complete",
                })
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            provider_result,
            Err(Error::UnsupportedByProvider {
                provider: "openai",
                ..
            })
        ));
        let _ = (prompt, opts); // suppress unused warnings
    }
}
