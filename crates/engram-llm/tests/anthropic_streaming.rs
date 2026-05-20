//! End-to-end wiremock test for `AnthropicProvider::complete_streamed`.
//! Verifies the request shape (`stream: true`, `accept: text/event-stream`),
//! that deltas reach the consumer in order, and that the terminal Done
//! carries the correct usage.

use std::sync::Arc;

use engram_llm::{
    anthropic::AnthropicProvider, CompleteOptions, LlmProvider, Model, PromptStructured,
    StreamChunk,
};
use engram_secrets::{MockStore, SecretsStore};
use futures_util::StreamExt;
use secrecy::Secret;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, AnthropicProvider) {
    let server = MockServer::start().await;
    let secrets: Arc<dyn SecretsStore> = Arc::new(MockStore::new());
    secrets
        .set("anthropic", Secret::new("sk-test".into()))
        .unwrap();
    let provider = AnthropicProvider::new(secrets, server.uri()).unwrap();
    (server, provider)
}

const CANNED_SSE: &str = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-3-5-haiku-20241022\",\"usage\":{\"input_tokens\":12,\"output_tokens\":0,\"cache_read_input_tokens\":4}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"streamed \"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"reply\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":9}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

#[tokio::test]
async fn streamed_round_trip_delivers_deltas_then_done() {
    let (server, provider) = setup().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("accept", "text/event-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(CANNED_SSE)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let prompt = PromptStructured::new("HEAD", "TAIL");
    let model = Model::anthropic("claude-3-5-haiku-20241022");

    let mut stream = provider
        .complete_streamed(&prompt, &model, &CompleteOptions::default())
        .await
        .expect("complete_streamed succeeds");

    let mut text = String::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.expect("no stream errors") {
            StreamChunk::Delta { text: d } => text.push_str(&d),
            StreamChunk::ToolUseMarker => {}
            StreamChunk::Done {
                usage,
                model_used,
                latency_ms: _,
                cost: _,
            } => {
                done = Some((usage, model_used));
            }
        }
    }

    assert_eq!(text, "streamed reply");
    let (usage, model_used) = done.expect("Done emitted");
    assert_eq!(model_used, "anthropic/claude-3-5-haiku-20241022");
    // 12 input + 4 cache_read = 16 total; output 9 (from message_delta)
    assert_eq!(usage.input_tokens_total, 16);
    assert_eq!(usage.input_tokens_cached, 4);
    assert_eq!(usage.output_tokens, 9);
}

#[tokio::test]
async fn streamed_maps_non_2xx_to_status_error() {
    let (server, provider) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "try again" }
        })))
        .mount(&server)
        .await;

    let result = provider
        .complete_streamed(
            &PromptStructured::dynamic_only("x"),
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions::default(),
        )
        .await;

    match result {
        Err(engram_llm::Error::Status { status, message }) => {
            assert_eq!(status, 503);
            assert_eq!(message, "try again");
        }
        Err(e) => panic!("expected Status, got error {e:?}"),
        Ok(_) => panic!("expected Status error, got Ok(stream)"),
    }
}

#[tokio::test]
async fn streamed_request_body_carries_stream_flag() {
    let (server, provider) = setup().await;

    let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = captured.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            *captured_clone.lock().unwrap() = Some(body);
            ResponseTemplate::new(200)
                .set_body_string(CANNED_SSE)
                .insert_header("content-type", "text/event-stream")
        })
        .mount(&server)
        .await;

    let mut stream = provider
        .complete_streamed(
            &PromptStructured::new("HEAD", "TAIL"),
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions::default(),
        )
        .await
        .unwrap();
    while let Some(_chunk) = stream.next().await {}

    let body = captured.lock().unwrap().clone().expect("body captured");
    assert_eq!(body["stream"], true);
}
