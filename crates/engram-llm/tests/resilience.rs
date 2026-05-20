//! Integration tests for the [`engram_llm`] resilience decorators.
//!
//! Covers acceptance criteria from #22:
//!
//! - retry honors the configured budget and re-tries transient failures;
//! - permanent failures fail fast (no retry);
//! - circuit breaker opens after 5 consecutive failures and short-circuits
//!   subsequent calls without invoking the inner provider;
//! - half-open trial behaviour after cooldown elapses;
//! - timeout wrapper returns `Error::Timeout` when the inner call hangs
//!   beyond the configured budget.
//!
//! The mock provider is a scripted, call-counting `LlmProvider` impl that
//! returns whatever the test queues up. No network, no real provider — these
//! tests are fast.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use engram_llm::{
    CircuitBreakerConfig, CircuitBreakerProvider, CircuitState, CompleteOptions, Completion, Error,
    LlmProvider, Model, PromptStructured, RetryConfig, RetryProvider, StreamedCompletion,
    TimeoutConfig, TimeoutProvider, Usage,
};
use engram_llm::{EmbeddingModel, Result};

// ─── Mock provider ─────────────────────────────────────────────────────────

/// Recipe for the next `complete` call.
enum Step {
    /// Resolve `Ok(default Completion)`.
    Ok,
    /// Resolve `Err(...)` with the given error.
    Err(Error),
    /// Sleep for `Duration` then resolve `Ok(default Completion)`.
    Sleep(Duration),
}

#[derive(Default)]
struct Mock {
    script: Mutex<Vec<Step>>,
    calls: AtomicUsize,
}

impl Mock {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(steps),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn ok_completion() -> Completion {
        Completion {
            text: "ok".to_string(),
            usage: Usage::default(),
            cost: engram_llm::Cost::unknown(),
            model_used: "mock/echo".to_string(),
            latency_ms: 0,
        }
    }
}

#[async_trait]
impl LlmProvider for Mock {
    async fn complete(
        &self,
        _: &PromptStructured,
        _: &Model,
        _: &CompleteOptions,
    ) -> Result<Completion> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = {
            let mut s = self.script.lock().unwrap();
            if s.is_empty() {
                return Ok(Mock::ok_completion());
            }
            s.remove(0)
        };
        match step {
            Step::Ok => Ok(Mock::ok_completion()),
            Step::Err(e) => Err(e),
            Step::Sleep(d) => {
                tokio::time::sleep(d).await;
                Ok(Mock::ok_completion())
            }
        }
    }
    async fn complete_streamed(
        &self,
        _: &PromptStructured,
        _: &Model,
        _: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        unreachable!("streamed not exercised in these tests")
    }
    async fn embed(&self, _: &str, _: &EmbeddingModel) -> Result<Vec<f32>> {
        unreachable!("embed not exercised in these tests")
    }
}

fn prompt() -> PromptStructured {
    PromptStructured::dynamic_only("hi")
}
fn model() -> Model {
    Model::anthropic("claude-3-5-haiku-20241022")
}
fn opts() -> CompleteOptions {
    CompleteOptions::default()
}

// `Arc<Mock>` cannot be wrapped by the decorators directly because the
// decorators take `P: LlmProvider` by value. This thin wrapper delegates
// through the `Arc` so the test can keep a handle for assertions.
struct ArcProvider<P: LlmProvider>(Arc<P>);

#[async_trait]
impl<P: LlmProvider> LlmProvider for ArcProvider<P> {
    async fn complete(
        &self,
        p: &PromptStructured,
        m: &Model,
        o: &CompleteOptions,
    ) -> Result<Completion> {
        self.0.complete(p, m, o).await
    }
    async fn complete_streamed(
        &self,
        p: &PromptStructured,
        m: &Model,
        o: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        self.0.complete_streamed(p, m, o).await
    }
    async fn embed(&self, text: &str, m: &EmbeddingModel) -> Result<Vec<f32>> {
        self.0.embed(text, m).await
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn retry_succeeds_on_second_attempt_after_transient_5xx() {
    let mock = Mock::new(vec![
        Step::Err(Error::Status {
            status: 503,
            message: "tmp".into(),
        }),
        Step::Ok,
    ]);

    let provider = RetryProvider::with_config(
        ArcProvider(mock.clone()),
        // Tight timers so the test is quick.
        RetryConfig {
            max_attempts: 4,
            base: Duration::from_millis(1),
            factor: 2,
            max_delay: Duration::from_millis(10),
            total_budget: Duration::from_secs(5),
        },
    );

    let out = provider.complete(&prompt(), &model(), &opts()).await;
    assert!(out.is_ok(), "expected Ok after retry, got {out:?}");
    assert_eq!(mock.calls(), 2, "expected exactly two underlying calls");
}

#[tokio::test]
async fn retry_does_not_retry_on_permanent_4xx() {
    let mock = Mock::new(vec![
        Step::Err(Error::Status {
            status: 401,
            message: "auth".into(),
        }),
        Step::Ok, // Should never be reached.
    ]);

    let provider = RetryProvider::with_config(
        ArcProvider(mock.clone()),
        RetryConfig {
            max_attempts: 4,
            base: Duration::from_millis(1),
            factor: 2,
            max_delay: Duration::from_millis(10),
            total_budget: Duration::from_secs(5),
        },
    );

    let err = provider
        .complete(&prompt(), &model(), &opts())
        .await
        .unwrap_err();
    match err {
        Error::Status { status: 401, .. } => {}
        other => panic!("expected raw 401 to bubble, got {other:?}"),
    }
    assert_eq!(mock.calls(), 1, "permanent failure must not retry");
}

#[tokio::test]
async fn retry_exhausts_budget_after_max_attempts() {
    let mock = Mock::new(vec![
        Step::Err(Error::EmptyResponse),
        Step::Err(Error::EmptyResponse),
        Step::Err(Error::EmptyResponse),
        Step::Err(Error::EmptyResponse),
    ]);

    let provider = RetryProvider::with_config(
        ArcProvider(mock.clone()),
        RetryConfig {
            max_attempts: 4,
            base: Duration::from_millis(1),
            factor: 2,
            max_delay: Duration::from_millis(2),
            total_budget: Duration::from_secs(5),
        },
    );

    let err = provider
        .complete(&prompt(), &model(), &opts())
        .await
        .unwrap_err();
    match err {
        Error::RetryBudgetExhausted { attempts, .. } => assert_eq!(attempts, 4),
        other => panic!("expected RetryBudgetExhausted, got {other:?}"),
    }
    assert_eq!(mock.calls(), 4);
}

#[tokio::test]
async fn breaker_opens_after_five_consecutive_5xx_and_short_circuits() {
    let mut script = Vec::new();
    for _ in 0..5 {
        script.push(Step::Err(Error::Status {
            status: 503,
            message: "boom".into(),
        }));
    }
    let mock = Mock::new(script);

    let provider = CircuitBreakerProvider::with_config(
        ArcProvider(mock.clone()),
        "mock",
        CircuitBreakerConfig {
            consecutive_failure_threshold: 5,
            failure_window_threshold: 99,
            ..Default::default()
        },
    );

    // First five calls each fail with the underlying 503.
    for i in 0..5 {
        let err = provider
            .complete(&prompt(), &model(), &opts())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Status { status: 503, .. }),
            "call {i}: expected 503, got {err:?}"
        );
    }
    assert_eq!(provider.state(), CircuitState::Open);
    assert_eq!(
        mock.calls(),
        5,
        "all five 503-returning calls must reach inner"
    );

    // The sixth call must be short-circuited without invoking the inner
    // provider — this is the core acceptance test.
    let err = provider
        .complete(&prompt(), &model(), &opts())
        .await
        .unwrap_err();
    match err {
        Error::CircuitBreakerOpen { provider, .. } => assert_eq!(provider, "mock"),
        other => panic!("expected CircuitBreakerOpen, got {other:?}"),
    }
    assert_eq!(
        mock.calls(),
        5,
        "call after the breaker tripped must not reach inner"
    );
}

#[tokio::test]
async fn breaker_admits_half_open_trial_after_cooldown_then_closes_on_success() {
    let mock = Mock::new(vec![
        Step::Err(Error::Status {
            status: 503,
            message: "x".into(),
        }),
        Step::Ok, // Becomes the trial after cooldown.
    ]);

    let provider = CircuitBreakerProvider::with_config(
        ArcProvider(mock.clone()),
        "mock",
        CircuitBreakerConfig {
            consecutive_failure_threshold: 1,
            failure_window_threshold: 99,
            cooldown: Duration::from_millis(20),
            max_cooldown: Duration::from_secs(1),
            ..Default::default()
        },
    );

    // Trip → Open.
    let _ = provider.complete(&prompt(), &model(), &opts()).await;
    assert_eq!(provider.state(), CircuitState::Open);

    // Wait past cooldown; next call is the trial.
    tokio::time::sleep(Duration::from_millis(40)).await;
    let out = provider.complete(&prompt(), &model(), &opts()).await;
    assert!(out.is_ok(), "trial should succeed; got {out:?}");
    assert_eq!(provider.state(), CircuitState::Closed);
}

#[tokio::test]
async fn timeout_returns_timeout_error_when_inner_hangs() {
    let mock = Mock::new(vec![Step::Sleep(Duration::from_millis(200))]);

    let provider = TimeoutProvider::with_config(
        ArcProvider(mock.clone()),
        TimeoutConfig {
            complete: Duration::from_millis(20),
            embed: Duration::from_secs(30),
        },
    );

    let err = provider
        .complete(&prompt(), &model(), &opts())
        .await
        .unwrap_err();
    match err {
        Error::Timeout { millis } => assert_eq!(millis, 20),
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn resilient_stack_retries_under_timeout_under_breaker() {
    // Compose all three layers via the convenience helper. Use tight timers
    // so the test runs in milliseconds.
    let mock = Mock::new(vec![
        Step::Err(Error::EmptyResponse), // attempt 1
        Step::Err(Error::EmptyResponse), // attempt 2
        Step::Ok,                        // attempt 3
    ]);

    let timeout = TimeoutProvider::with_config(
        ArcProvider(mock.clone()),
        TimeoutConfig {
            complete: Duration::from_secs(5),
            embed: Duration::from_secs(5),
        },
    );
    let retry = RetryProvider::with_config(
        timeout,
        RetryConfig {
            max_attempts: 4,
            base: Duration::from_millis(1),
            factor: 2,
            max_delay: Duration::from_millis(5),
            total_budget: Duration::from_secs(5),
        },
    );
    let breaker = CircuitBreakerProvider::with_config(
        retry,
        "mock",
        CircuitBreakerConfig {
            consecutive_failure_threshold: 99,
            failure_window_threshold: 99,
            ..Default::default()
        },
    );

    let out = breaker.complete(&prompt(), &model(), &opts()).await;
    assert!(out.is_ok(), "expected eventual success, got {out:?}");
    assert_eq!(mock.calls(), 3);
    assert_eq!(breaker.state(), CircuitState::Closed);
}
