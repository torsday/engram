//! Per-provider circuit breaker decorator for [`crate::LlmProvider`].
//!
//! Wraps an inner provider in a three-state circuit (closed / open /
//! half-open) so the runtime fails fast when an upstream is broken instead
//! of hammering it.
//!
//! # State machine
//!
//! ```text
//!             ┌────────┐ failures ≥ threshold  ┌──────┐
//!             │ Closed ├──────────────────────►│ Open │
//!             └────▲───┘                       └──┬───┘
//!                  │                              │ cooldown elapses
//!                  │                              ▼
//!                  │  trial succeeds       ┌────────────┐
//!                  └───────────────────────┤ Half-open  │
//!                                          └──────┬─────┘
//!                                                 │ trial fails
//!                                                 ▼
//!                                          (open with cooldown × 2)
//! ```
//!
//! - **Closed → Open** when *either* the rolling window contains
//!   `failure_window_threshold` failures within `failure_window` *or* the
//!   consecutive-failure counter hits `consecutive_failure_threshold`.
//! - **Open → Half-open** automatically after `cooldown`. The next call is
//!   admitted as the trial; subsequent concurrent calls return
//!   `CircuitBreakerOpen` immediately until the trial resolves.
//! - **Half-open → Closed** on trial success (resets cooldown and counters).
//! - **Half-open → Open** on trial failure, doubling the cooldown up to
//!   `max_cooldown`.
//!
//! Only failures whose category satisfies
//! [`engram_core::error::ErrorCategory::counts_toward_breaker`] increment the
//! failure counter — operator-config errors don't open the breaker.
//!
//! # Tracing
//!
//! Every state transition emits `tracing::warn!` (closed → open / cooldown
//! double) or `tracing::info!` (open → half-open / half-open → closed) with
//! the provider name and counters, fulfilling the issue's "Pacekeeper reads
//! circuit-breaker state via a tracing event" acceptance criterion.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use engram_core::error::ErrorCategory;

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::StreamedCompletion;
use crate::types::{CompleteOptions, Completion, EmbeddingModel, Model, PromptStructured};

/// Tunable circuit-breaker parameters. Defaults match
/// `docs/design/03-architecture.md` §Circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Rolling-window length for the "≥ N failures in M seconds" rule.
    pub failure_window: Duration,
    /// Failure count within `failure_window` that flips Closed → Open.
    pub failure_window_threshold: u32,
    /// Consecutive-failure count that flips Closed → Open, regardless of
    /// the rolling window.
    pub consecutive_failure_threshold: u32,
    /// Initial cooldown duration when opening.
    pub cooldown: Duration,
    /// Cap on cooldown when consecutive opens occur in Half-open.
    pub max_cooldown: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_window: Duration::from_secs(60),
            failure_window_threshold: 10,
            consecutive_failure_threshold: 5,
            cooldown: Duration::from_secs(30),
            max_cooldown: Duration::from_secs(300),
        }
    }
}

/// Public view of the breaker's current state. Used by tests and by callers
/// that want to expose state via metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Calls flow through to the inner provider.
    Closed,
    /// Calls are rejected immediately with [`Error::CircuitBreakerOpen`].
    Open,
    /// Next call is admitted as a trial; subsequent concurrent calls are
    /// rejected until the trial resolves.
    HalfOpen,
}

/// Internal mutable state — held under a single `Mutex` because every
/// transition reads and writes multiple fields atomically.
#[derive(Debug)]
struct State {
    state: CircuitState,
    /// Timestamps of recent failures, oldest first. Pruned to
    /// `failure_window` on each update.
    failures: VecDeque<Instant>,
    consecutive_failures: u32,
    /// When the breaker opened (so we know when cooldown elapses).
    opened_at: Option<Instant>,
    /// Current cooldown — starts at `cooldown`, doubles on consecutive opens,
    /// capped at `max_cooldown`.
    current_cooldown: Duration,
    /// True if a trial call is currently in flight (Half-open admits one).
    trial_in_flight: bool,
}

impl State {
    fn new(initial_cooldown: Duration) -> Self {
        Self {
            state: CircuitState::Closed,
            failures: VecDeque::new(),
            consecutive_failures: 0,
            opened_at: None,
            current_cooldown: initial_cooldown,
            trial_in_flight: false,
        }
    }
}

/// A [`LlmProvider`] decorator implementing the breaker described in the
/// module docs.
///
/// `provider_name` is purely a label used in error messages and tracing.
pub struct CircuitBreakerProvider<P: LlmProvider> {
    inner: P,
    provider_name: String,
    config: CircuitBreakerConfig,
    state: Mutex<State>,
}

impl<P: LlmProvider> CircuitBreakerProvider<P> {
    /// Wrap `inner` with a breaker using default thresholds.
    pub fn new(inner: P, provider_name: impl Into<String>) -> Self {
        Self::with_config(inner, provider_name, CircuitBreakerConfig::default())
    }

    /// Wrap `inner` with explicit thresholds. Used by tests with shortened
    /// timers.
    pub fn with_config(
        inner: P,
        provider_name: impl Into<String>,
        config: CircuitBreakerConfig,
    ) -> Self {
        let state = Mutex::new(State::new(config.cooldown));
        Self {
            inner,
            provider_name: provider_name.into(),
            config,
            state,
        }
    }

    /// Borrow the underlying provider.
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// Read the current state — for metrics and tests.
    pub fn state(&self) -> CircuitState {
        self.state.lock().expect("breaker state mutex").state
    }

    /// Check whether the call is admitted right now. If the breaker is Open
    /// and cooldown has elapsed, transitions to Half-open and admits exactly
    /// one trial. Returns the cooldown remaining (in ms) when rejecting.
    ///
    /// `now` is injectable for tests; production calls pass `Instant::now()`.
    fn admit(&self, now: Instant) -> std::result::Result<(), u64> {
        let mut s = self.state.lock().expect("breaker state mutex");
        match s.state {
            CircuitState::Closed => Ok(()),
            CircuitState::Open => {
                let opened_at = s.opened_at.expect("Open with no opened_at");
                let elapsed = now.duration_since(opened_at);
                if elapsed >= s.current_cooldown {
                    // Cooldown elapsed → admit trial.
                    s.state = CircuitState::HalfOpen;
                    s.trial_in_flight = true;
                    tracing::info!(
                        provider = %self.provider_name,
                        cooldown_ms = s.current_cooldown.as_millis() as u64,
                        "circuit breaker: open → half-open (trial admitted)"
                    );
                    Ok(())
                } else {
                    let remaining = s.current_cooldown - elapsed;
                    Err(remaining.as_millis() as u64)
                }
            }
            CircuitState::HalfOpen => {
                if s.trial_in_flight {
                    // Another trial is in flight; reject this one.
                    Err(0)
                } else {
                    s.trial_in_flight = true;
                    Ok(())
                }
            }
        }
    }

    /// Record a success. Closes the breaker if it was Half-open; resets
    /// counters either way.
    fn record_success(&self) {
        let mut s = self.state.lock().expect("breaker state mutex");
        let was_half_open = matches!(s.state, CircuitState::HalfOpen);
        s.state = CircuitState::Closed;
        s.failures.clear();
        s.consecutive_failures = 0;
        s.opened_at = None;
        s.current_cooldown = self.config.cooldown;
        s.trial_in_flight = false;
        if was_half_open {
            tracing::info!(
                provider = %self.provider_name,
                "circuit breaker: half-open → closed (trial succeeded; counters reset)"
            );
        }
    }

    /// Record a counted failure. Reads the current category to decide
    /// whether this failure flips the breaker.
    fn record_failure(&self, category: ErrorCategory, now: Instant) {
        if !category.counts_toward_breaker() {
            return;
        }
        let mut s = self.state.lock().expect("breaker state mutex");

        // Always end the trial-in-flight on a failure.
        let was_half_open = matches!(s.state, CircuitState::HalfOpen);
        s.trial_in_flight = false;

        if was_half_open {
            // Half-open trial failed → reopen with doubled cooldown.
            s.current_cooldown = (s.current_cooldown * 2).min(self.config.max_cooldown);
            s.opened_at = Some(now);
            s.state = CircuitState::Open;
            tracing::warn!(
                provider = %self.provider_name,
                new_cooldown_ms = s.current_cooldown.as_millis() as u64,
                "circuit breaker: half-open → open (trial failed; cooldown doubled)"
            );
            return;
        }

        s.consecutive_failures = s.consecutive_failures.saturating_add(1);
        s.failures.push_back(now);

        // Prune old failures outside the window.
        while let Some(front) = s.failures.front() {
            if now.duration_since(*front) > self.config.failure_window {
                s.failures.pop_front();
            } else {
                break;
            }
        }

        let window_trip = s.failures.len() as u32 >= self.config.failure_window_threshold;
        let consecutive_trip = s.consecutive_failures >= self.config.consecutive_failure_threshold;

        if matches!(s.state, CircuitState::Closed) && (window_trip || consecutive_trip) {
            s.state = CircuitState::Open;
            s.opened_at = Some(now);
            tracing::warn!(
                provider = %self.provider_name,
                failures_in_window = s.failures.len(),
                consecutive_failures = s.consecutive_failures,
                cooldown_ms = s.current_cooldown.as_millis() as u64,
                "circuit breaker: closed → open"
            );
        }
    }

    /// Wrap a single async operation with admit / record-success /
    /// record-failure. Generic so every trait method reuses it.
    async fn execute<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let now = Instant::now();
        if let Err(cooldown_ms) = self.admit(now) {
            return Err(Error::CircuitBreakerOpen {
                provider: self.provider_name.clone(),
                cooldown_ms,
            });
        }
        let outcome = op().await;
        match &outcome {
            Ok(_) => self.record_success(),
            Err(e) => self.record_failure(e.category(), Instant::now()),
        }
        outcome
    }
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for CircuitBreakerProvider<P> {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        self.execute(|| self.inner.complete(prompt, model, options))
            .await
    }

    async fn complete_streamed(
        &self,
        prompt: &PromptStructured,
        model: &Model,
        options: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        // Note: only the initial request is breaker-gated; mid-stream
        // failures don't update the breaker (the call already counted as a
        // success at the moment the stream was returned).
        self.execute(|| self.inner.complete_streamed(prompt, model, options))
            .await
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        self.execute(|| self.inner.embed(text, model)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Failure-counting on a fake provider is exercised in `tests/breaker.rs`
    // via the shared MockProvider. Here we keep small pure-state tests so
    // the breaker logic is covered without spawning futures.

    fn mk(config: CircuitBreakerConfig) -> CircuitBreakerProvider<NoopProvider> {
        CircuitBreakerProvider::with_config(NoopProvider, "test", config)
    }

    // A trivial inner provider used only so the breaker has something to
    // wrap; admit/record APIs are called directly without invoking the
    // trait methods.
    struct NoopProvider;
    #[async_trait]
    impl LlmProvider for NoopProvider {
        async fn complete(
            &self,
            _: &PromptStructured,
            _: &Model,
            _: &CompleteOptions,
        ) -> Result<Completion> {
            unreachable!()
        }
        async fn complete_streamed(
            &self,
            _: &PromptStructured,
            _: &Model,
            _: &CompleteOptions,
        ) -> Result<StreamedCompletion> {
            unreachable!()
        }
        async fn embed(&self, _: &str, _: &EmbeddingModel) -> Result<Vec<f32>> {
            unreachable!()
        }
    }

    #[test]
    fn opens_after_consecutive_threshold_hits() {
        let cb = mk(CircuitBreakerConfig {
            consecutive_failure_threshold: 3,
            failure_window_threshold: 999, // disabled for this test
            ..Default::default()
        });
        let t = Instant::now();
        for _ in 0..3 {
            cb.record_failure(ErrorCategory::Transient, t);
        }
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn opens_after_window_threshold_hits() {
        let cb = mk(CircuitBreakerConfig {
            consecutive_failure_threshold: 999, // disabled for this test
            failure_window_threshold: 3,
            failure_window: Duration::from_secs(60),
            ..Default::default()
        });
        let t = Instant::now();
        for _ in 0..3 {
            cb.record_failure(ErrorCategory::Transient, t);
        }
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn admit_transitions_open_to_half_open_after_cooldown() {
        let cb = mk(CircuitBreakerConfig {
            consecutive_failure_threshold: 1,
            cooldown: Duration::from_millis(50),
            ..Default::default()
        });
        let t0 = Instant::now();
        cb.record_failure(ErrorCategory::Transient, t0);
        assert_eq!(cb.state(), CircuitState::Open);

        // Before cooldown: rejected.
        let admit_early = cb.admit(t0 + Duration::from_millis(10));
        assert!(admit_early.is_err());

        // After cooldown: admitted; state moves to half-open.
        let admit_late = cb.admit(t0 + Duration::from_millis(100));
        assert!(admit_late.is_ok());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn half_open_trial_failure_doubles_cooldown() {
        let cb = mk(CircuitBreakerConfig {
            consecutive_failure_threshold: 1,
            cooldown: Duration::from_millis(50),
            max_cooldown: Duration::from_secs(1),
            ..Default::default()
        });
        let t0 = Instant::now();

        // Trip → Open.
        cb.record_failure(ErrorCategory::Transient, t0);
        // Admit trial → Half-open.
        cb.admit(t0 + Duration::from_millis(100)).unwrap();
        // Trial fails → back to Open with doubled cooldown (100ms).
        cb.record_failure(ErrorCategory::Transient, t0 + Duration::from_millis(110));
        assert_eq!(cb.state(), CircuitState::Open);
        assert_eq!(
            cb.state.lock().unwrap().current_cooldown,
            Duration::from_millis(100)
        );
    }

    #[test]
    fn half_open_trial_success_returns_to_closed() {
        let cb = mk(CircuitBreakerConfig {
            consecutive_failure_threshold: 1,
            cooldown: Duration::from_millis(50),
            ..Default::default()
        });
        let t0 = Instant::now();
        cb.record_failure(ErrorCategory::Transient, t0);
        cb.admit(t0 + Duration::from_millis(100)).unwrap();
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        // Cooldown is reset to the configured base.
        assert_eq!(
            cb.state.lock().unwrap().current_cooldown,
            Duration::from_millis(50)
        );
    }

    #[test]
    fn system_category_failures_do_not_open_breaker() {
        let cb = mk(CircuitBreakerConfig {
            consecutive_failure_threshold: 1,
            ..Default::default()
        });
        let t = Instant::now();
        cb.record_failure(ErrorCategory::System, t);
        cb.record_failure(ErrorCategory::Permanent, t);
        assert_eq!(cb.state(), CircuitState::Closed);
    }
}
