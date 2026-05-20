//! Live Anthropic API smoke test — gated by `ENGRAM_REAL_LLM=1`.
//!
//! Runs in the nightly CI workflow when an API key is wired in via
//! `ANTHROPIC_API_KEY`; never in the default `cargo test` path. Skips with
//! a printed reason when neither the gate nor the secret is set.
//!
//! What it verifies:
//! - The pinned API version (`2023-06-01`) is still accepted by the live
//!   service.
//! - A real Haiku request returns a non-empty text completion.
//! - Token usage is reported in the shape our parser expects.

use std::sync::Arc;

use engram_llm::{
    anthropic::AnthropicProvider, CompleteOptions, LlmProvider, Model, PromptStructured,
};
use engram_secrets::{MockStore, SecretsStore};
use secrecy::Secret;

fn live_enabled() -> Option<String> {
    if std::env::var("ENGRAM_REAL_LLM").as_deref() != Ok("1") {
        eprintln!("ENGRAM_REAL_LLM=1 not set — skipping live Anthropic call");
        return None;
    }
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.is_empty() => Some(k),
        _ => {
            eprintln!("ANTHROPIC_API_KEY not set — skipping live Anthropic call");
            None
        }
    }
}

#[tokio::test]
async fn live_haiku_smoke() {
    let Some(key) = live_enabled() else { return };

    let secrets: Arc<dyn SecretsStore> = Arc::new(MockStore::new());
    secrets.set("anthropic", Secret::new(key)).unwrap();

    let provider = AnthropicProvider::new(secrets, AnthropicProvider::DEFAULT_BASE_URL).unwrap();

    let prompt = PromptStructured::new(
        "You are a brief assistant. Reply in fewer than ten words.",
        "Say hi.",
    );

    let resp = provider
        .complete(
            &prompt,
            &Model::anthropic("claude-3-5-haiku-20241022"),
            &CompleteOptions {
                temperature: 0.0,
                max_tokens: 32,
                ..Default::default()
            },
        )
        .await
        .expect("live Haiku call succeeds");

    assert!(!resp.text.is_empty(), "expected non-empty text");
    assert!(
        resp.usage.input_tokens_total > 0,
        "expected non-zero input tokens"
    );
    assert!(
        resp.usage.output_tokens > 0,
        "expected non-zero output tokens"
    );
    eprintln!(
        "live haiku ok: input={} cached={} output={} latency={}ms",
        resp.usage.input_tokens_total,
        resp.usage.input_tokens_cached,
        resp.usage.output_tokens,
        resp.latency_ms
    );
}
