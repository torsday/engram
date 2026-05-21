//! Ollama provider — local LLM inference + bge-m3 embeddings.
//!
//! ## Completions
//!
//! Uses POST `/api/chat` with Ollama's NDJSON streaming protocol. The
//! structured prompt's `static_head` and `dynamic_tail` are concatenated into
//! a single user message. Ollama does not support prompt caching, so
//! `input_tokens_cached` and `input_tokens_cache_create` are always `0`.
//!
//! Non-streaming completion uses the same endpoint with `"stream": false`.
//!
//! ## Embeddings
//!
//! Uses POST `/api/embed`. The default embedding model is `bge-m3` (1024
//! dimensions per ADR 0014). The caller selects the model via
//! [`EmbeddingModel`].
//!
//! ## Cost
//!
//! Always `0.0` — local inference has no dollar cost. Energy cost is
//! intentionally ignored (out of scope for v1).
//!
//! ## Authentication
//!
//! Ollama runs unauthenticated by default. No API key is required.

use std::time::Instant;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::{StreamChunk, StreamedCompletion};
use crate::types::{
    CompleteOptions, Completion, Cost, EmbeddingModel, Model, ModelProvider, PromptStructured,
    Usage,
};

/// Default base URL for a local Ollama instance.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Ollama [`LlmProvider`] implementation.
///
/// Talks to a local (or remote) Ollama instance via its HTTP API.
/// No API key is required.
pub struct OllamaProvider {
    http: Client,
    base_url: String,
}

impl OllamaProvider {
    /// Build a provider pointing at `base_url`.
    ///
    /// Pass [`DEFAULT_BASE_URL`] in production; tests pass a mock server URL.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("engram-llm/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.into(),
        })
    }

    fn require_ollama(&self, model: &Model, op: &'static str) -> Result<()> {
        if model.provider != ModelProvider::Ollama {
            return Err(Error::UnsupportedByProvider {
                provider: "ollama",
                op,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        self.require_ollama(model, "complete")?;

        let full_prompt = format!("{}{}", prompt.static_head, prompt.dynamic_tail);
        let body = build_chat_body(&full_prompt, model, options, false);

        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    Error::ProviderUnavailable {
                        provider: "ollama",
                        message: format!("cannot connect to Ollama at {}: {e}", self.base_url),
                    }
                } else {
                    e.into()
                }
            })?;

        let status = response.status();
        let body_text = response.text().await?;
        let latency_ms = started.elapsed().as_millis() as u64;

        if !status.is_success() {
            let message = parse_ollama_error(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: ChatResponse = serde_json::from_str(&body_text)
            .map_err(|e| Error::Decode(format!("ollama /api/chat response: {e}")))?;

        let text = parsed.message.content.trim().to_owned();
        if text.is_empty() {
            return Err(Error::EmptyResponse);
        }

        let usage = Usage {
            input_tokens_total: parsed.prompt_eval_count.unwrap_or(0),
            input_tokens_cached: 0,
            input_tokens_cache_create: 0,
            output_tokens: parsed.eval_count.unwrap_or(0),
        };

        let model_used = format!("ollama/{}", model.name);

        tracing::info!(
            target: "engram_llm::ollama",
            model = %model_used,
            input_tokens_total = usage.input_tokens_total,
            output_tokens = usage.output_tokens,
            latency_ms,
            "ollama.complete ok"
        );

        Ok(Completion {
            text,
            usage,
            cost: Cost::unknown(), // local inference: $0
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
        self.require_ollama(model, "complete_streamed")?;

        let full_prompt = format!("{}{}", prompt.static_head, prompt.dynamic_tail);
        let body = build_chat_body(&full_prompt, model, options, true);

        let started = Instant::now();
        let response = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .header("content-type", "application/json")
            .header("accept", "application/x-ndjson")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    Error::ProviderUnavailable {
                        provider: "ollama",
                        message: format!("cannot connect to Ollama at {}: {e}", self.base_url),
                    }
                } else {
                    e.into()
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await?;
            let message = parse_ollama_error(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let model_name = model.name.clone();
        let bytes = Box::pin(response.bytes_stream());
        let stream = futures_util::stream::unfold(
            OllamaStreamState {
                lines: bytes,
                model_name: model_name.clone(),
                started,
                input_tokens: 0,
                output_tokens: 0,
                done_emitted: false,
                buf: String::new(),
            },
            |mut state| async move {
                if state.done_emitted {
                    return None;
                }
                loop {
                    use futures_util::StreamExt as _;
                    let chunk = state.lines.next().await?;
                    let bytes = match chunk {
                        Ok(b) => b,
                        Err(e) => {
                            return Some((Err(Error::from(e)), state));
                        }
                    };
                    state.buf.push_str(&String::from_utf8_lossy(&bytes));

                    // Process all complete newline-delimited JSON objects in buf.
                    while let Some(nl) = state.buf.find('\n') {
                        let line = state.buf[..nl].trim().to_owned();
                        state.buf = state.buf[nl + 1..].to_owned();

                        if line.is_empty() {
                            continue;
                        }

                        let obj: serde_json::Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => {
                                return Some((
                                    Err(Error::Decode(format!("ollama ndjson: {e}"))),
                                    state,
                                ));
                            }
                        };

                        let done = obj["done"].as_bool().unwrap_or(false);

                        if !done {
                            // Delta chunk.
                            let delta = obj["message"]["content"].as_str().unwrap_or("").to_owned();
                            if !delta.is_empty() {
                                return Some((Ok(StreamChunk::Delta { text: delta }), state));
                            }
                        } else {
                            // Final chunk — extract usage.
                            state.input_tokens =
                                obj["prompt_eval_count"].as_u64().unwrap_or(0) as u32;
                            state.output_tokens = obj["eval_count"].as_u64().unwrap_or(0) as u32;
                            state.done_emitted = true;

                            let usage = Usage {
                                input_tokens_total: state.input_tokens,
                                input_tokens_cached: 0,
                                input_tokens_cache_create: 0,
                                output_tokens: state.output_tokens,
                            };
                            let model_used = format!("ollama/{}", state.model_name);
                            return Some((
                                Ok(StreamChunk::Done {
                                    usage,
                                    cost: Cost::unknown(),
                                    model_used,
                                    latency_ms: state.started.elapsed().as_millis() as u64,
                                }),
                                state,
                            ));
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        // Ollama only serves Ollama-configured embedding models.
        if model.provider != ModelProvider::Ollama {
            return Err(Error::UnsupportedByProvider {
                provider: "ollama",
                op: "embed with non-ollama model",
            });
        }
        let model_name = model.name.as_str();

        let body = serde_json::json!({
            "model": model_name,
            "input": text,
        });

        let response = self
            .http
            .post(format!("{}/api/embed", self.base_url))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    Error::ProviderUnavailable {
                        provider: "ollama",
                        message: format!("cannot connect to Ollama at {}: {e}", self.base_url),
                    }
                } else {
                    e.into()
                }
            })?;

        let status = response.status();
        let body_text = response.text().await?;

        if !status.is_success() {
            let message = parse_ollama_error(&body_text)
                .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
            return Err(Error::Status {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: EmbedResponse = serde_json::from_str(&body_text)
            .map_err(|e| Error::Decode(format!("ollama /api/embed response: {e}")))?;

        parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or(Error::EmptyResponse)
    }
}

// ---------------------------------------------------------------------------
// Request / response wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<ChatOptions>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: String,
}

#[derive(Serialize)]
struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

// ---------------------------------------------------------------------------
// Streaming state
// ---------------------------------------------------------------------------

struct OllamaStreamState {
    lines: futures_util::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    model_name: String,
    started: Instant,
    input_tokens: u32,
    output_tokens: u32,
    done_emitted: bool,
    buf: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_chat_body<'a>(
    prompt: &'a str,
    model: &'a Model,
    options: &CompleteOptions,
    stream: bool,
) -> ChatRequest<'a> {
    ChatRequest {
        model: &model.name,
        messages: vec![ChatMessage {
            role: "user",
            content: prompt.to_owned(),
        }],
        stream,
        options: Some(ChatOptions {
            temperature: Some(options.temperature),
            num_predict: Some(options.max_tokens),
        }),
    }
}

/// Extract an error message from Ollama's error JSON body (best-effort).
fn parse_ollama_error(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v["error"].as_str().map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ollama_model(name: &str) -> Model {
        Model::ollama(name)
    }

    // --- serialization ---

    #[test]
    fn build_chat_body_produces_user_message() {
        let model = ollama_model("llama3.2");
        let opts = CompleteOptions::default();
        let body = build_chat_body("Hello!", &model, &opts, false);
        assert_eq!(body.model, "llama3.2");
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.messages[0].content, "Hello!");
        assert!(!body.stream);
        // options always included with defaults
        let o = body.options.unwrap();
        assert_eq!(o.temperature, Some(0.2));
        assert_eq!(o.num_predict, Some(1024));
    }

    #[test]
    fn build_chat_body_with_options() {
        let model = ollama_model("llama3.2");
        let opts = CompleteOptions {
            temperature: 0.7,
            max_tokens: 512,
            ..Default::default()
        };
        let body = build_chat_body("Hi", &model, &opts, true);
        assert!(body.stream);
        let o = body.options.unwrap();
        assert_eq!(o.temperature, Some(0.7));
        assert_eq!(o.num_predict, Some(512));
    }

    // --- deserialization ---

    #[test]
    fn chat_response_deserializes() {
        let json = r#"{"model":"llama3.2","created_at":"2024-08-22T20:00:00Z","message":{"role":"assistant","content":"Hello!"},"done":true,"prompt_eval_count":26,"eval_count":7}"#;
        let r: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.message.content, "Hello!");
        assert_eq!(r.prompt_eval_count, Some(26));
        assert_eq!(r.eval_count, Some(7));
    }

    #[test]
    fn chat_response_missing_counts_defaults_to_none() {
        let json =
            r#"{"model":"llama3.2","message":{"role":"assistant","content":"Hi"},"done":true}"#;
        let r: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.prompt_eval_count, None);
        assert_eq!(r.eval_count, None);
    }

    #[test]
    fn embed_response_deserializes() {
        let json = r#"{"model":"bge-m3","embeddings":[[0.1,0.2,0.3]]}"#;
        let r: EmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.embeddings.len(), 1);
        assert_eq!(r.embeddings[0], vec![0.1_f32, 0.2, 0.3]);
    }

    #[test]
    fn parse_ollama_error_extracts_message() {
        let body = r#"{"error":"model 'nope' not found"}"#;
        assert_eq!(
            parse_ollama_error(body),
            Some("model 'nope' not found".to_owned())
        );
    }

    #[test]
    fn parse_ollama_error_returns_none_for_unknown_shape() {
        assert_eq!(parse_ollama_error("not json"), None);
        assert_eq!(parse_ollama_error(r#"{"other":"field"}"#), None);
    }

    #[test]
    fn provider_rejects_non_ollama_model() {
        let provider = OllamaProvider::new(DEFAULT_BASE_URL).unwrap();
        let model = Model::anthropic("claude-3-5-haiku-20241022");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(provider.complete(
                &PromptStructured {
                    static_head: "".into(),
                    dynamic_tail: "hi".into(),
                },
                &model,
                &CompleteOptions::default(),
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            Error::UnsupportedByProvider {
                provider: "ollama",
                ..
            }
        ));
    }
}
