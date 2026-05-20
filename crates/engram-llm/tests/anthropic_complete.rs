//! Integration tests for `AnthropicProvider::complete` using a wiremock
//! server. These exercise the request shape, response parsing, error
//! mapping, and metric extraction end-to-end without ever touching the live
//! Anthropic API.

use std::sync::Arc;

use engram_llm::{
    anthropic::AnthropicProvider, CompleteOptions, LlmProvider, Model, PromptStructured,
};
use engram_secrets::{MockStore, SecretsStore};
use secrecy::Secret;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

async fn setup() -> (MockServer, AnthropicProvider) {
    let server = MockServer::start().await;
    let secrets: Arc<dyn SecretsStore> = Arc::new(MockStore::new());
    secrets
        .set("anthropic", Secret::new("sk-test".into()))
        .unwrap();
    let provider = AnthropicProvider::new(secrets, server.uri()).unwrap();
    (server, provider)
}

fn ok_response(model: &str, text: &str, input_tokens: u32, cache_read: u32) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "msg_test",
        "model": model,
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": 7,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": 0
        }
    }))
}

#[tokio::test]
async fn complete_round_trip_with_cache_marker() {
    let (server, provider) = setup().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ok_response(
            "claude-3-5-haiku-20241022",
            "hi there",
            100,
            80,
        ))
        .mount(&server)
        .await;

    let prompt = PromptStructured::new("you are an assistant", "say hi");
    let model = Model::anthropic("claude-3-5-haiku-20241022");
    let resp = provider
        .complete(&prompt, &model, &CompleteOptions::default())
        .await
        .expect("complete succeeds");

    assert_eq!(resp.text, "hi there");
    assert_eq!(resp.model_used, "anthropic/claude-3-5-haiku-20241022");
    // 100 input + 80 cache_read = 180 total; cached = 80 ⇒ ratio ~0.444
    assert_eq!(resp.usage.input_tokens_total, 180);
    assert_eq!(resp.usage.input_tokens_cached, 80);
    assert_eq!(resp.usage.output_tokens, 7);
    assert!((resp.usage.cache_hit_ratio() - (80.0 / 180.0)).abs() < 1e-4);
}

#[tokio::test]
async fn complete_sends_cache_control_marker_on_static_head() {
    let (server, provider) = setup().await;

    // Capture the request body and assert cache_control is present.
    let captured: std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_clone = captured.clone();

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            *captured_clone.lock().unwrap() = Some(body);
            ok_response("claude-3-5-haiku-20241022", "ok", 1, 0)
        })
        .mount(&server)
        .await;

    let prompt = PromptStructured::new("HEAD", "TAIL");
    provider
        .complete(
            &prompt,
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions::default(),
        )
        .await
        .unwrap();

    let body = captured
        .lock()
        .unwrap()
        .clone()
        .expect("captured request body");
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["text"], "HEAD");
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(content[1]["text"], "TAIL");
    assert!(content[1].get("cache_control").is_none());
}

#[tokio::test]
async fn complete_maps_non_2xx_to_status_error() {
    let (server, provider) = setup().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "type": "error",
            "error": { "type": "rate_limit_error", "message": "slow down" }
        })))
        .mount(&server)
        .await;

    let prompt = PromptStructured::dynamic_only("x");
    let result = provider
        .complete(
            &prompt,
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions::default(),
        )
        .await;

    match result {
        Err(engram_llm::Error::Status { status, message }) => {
            assert_eq!(status, 429);
            assert_eq!(message, "slow down");
        }
        other => panic!("expected Status error, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_maps_empty_content_to_empty_response() {
    let (server, provider) = setup().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_empty",
            "model": "claude-3-5-haiku-20241022",
            "role": "assistant",
            "content": [],
            "usage": { "input_tokens": 1, "output_tokens": 0 }
        })))
        .mount(&server)
        .await;

    let prompt = PromptStructured::dynamic_only("x");
    let result = provider
        .complete(
            &prompt,
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions::default(),
        )
        .await;

    assert!(matches!(result, Err(engram_llm::Error::EmptyResponse)));
}

#[tokio::test]
async fn complete_rejects_non_anthropic_model() {
    let (_server, provider) = setup().await;

    let result = provider
        .complete(
            &PromptStructured::dynamic_only("x"),
            &Model {
                provider: engram_llm::ModelProvider::OpenAi,
                name: "gpt-4o".into(),
            },
            &CompleteOptions::default(),
        )
        .await;

    assert!(matches!(
        result,
        Err(engram_llm::Error::UnsupportedByProvider {
            provider: "anthropic",
            op: "complete"
        })
    ));
}

#[tokio::test]
async fn embed_unsupported_on_anthropic() {
    let (_server, provider) = setup().await;
    let model = engram_llm::EmbeddingModel {
        provider: engram_llm::ModelProvider::Anthropic,
        name: "n/a".into(),
        dim: 1024,
    };
    let result = provider.embed("hello", &model).await;
    assert!(matches!(
        result,
        Err(engram_llm::Error::UnsupportedByProvider {
            provider: "anthropic",
            op: "embed"
        })
    ));
}

#[tokio::test]
async fn complete_surfaces_decode_error_on_malformed_body() {
    let (server, provider) = setup().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&server)
        .await;

    let result = provider
        .complete(
            &PromptStructured::dynamic_only("x"),
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions::default(),
        )
        .await;
    assert!(matches!(result, Err(engram_llm::Error::Decode(_))));
}
