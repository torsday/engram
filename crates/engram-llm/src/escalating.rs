//! Tiered model escalation per [ADR 0011].
//!
//! Wraps any [`LlmProvider`] in a policy that attempts the cheapest tier first
//! and escalates only when the response signals it's needed:
//!
//! - **`schema_invalid`** — the response fails caller-supplied validation
//! - **`confidence_below_<t>`** — the response's `confidence` field is below
//!   the configured threshold
//! - **`explicit_request`** — the response includes `"escalate": true`
//!
//! Costs across attempts are summed; the final returned [`Completion`] carries
//! the cumulative cost and aggregated [`Usage`], with `model_used` set to the
//! tier that produced the response. The number of attempts a single call took
//! is exposed via [`Completion::model_used`] (tier name) plus
//! [`EscalatingProvider::last_attempts_taken`] for agent-layer metric recording.
//!
//! # Out of scope
//!
//! - TOML config parsing — the agent layer constructs [`EscalationConfig`].
//! - `agent_actions` row recording — the agent layer captures `tier_used` and
//!   the attempt count from the returned [`Completion`] / introspection helpers.
//! - `schema_drift`-driven `start_tier` tuning — Watcher concern, lands in
//!   v2.1 (rolling 100-call schema-validity rate).
//! - Eval-framework integration — orthogonal to the per-call escalation logic.
//!
//! # Streaming + embedding
//!
//! Escalation is for **structured-output completions** — the trigger signals
//! live in the parsed JSON response. Streaming calls and embedding calls
//! pass through to the inner provider unchanged at the start tier.
//!
//! [ADR 0011]: ../docs/design/adrs/0011-tiered-model-escalation.md

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::provider::LlmProvider;
use crate::streaming::StreamedCompletion;
use crate::types::{
    CompleteOptions, Completion, Cost, EmbeddingModel, Model, PromptStructured, Usage,
};

/// Type alias for a boxed schema validator. Returns `true` if the response
/// parses against the agent's structured-output schema.
pub type SchemaValidator = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Static configuration for an escalation ladder.
///
/// Construct once per agent and share with the agent's [`EscalatingProvider`].
/// Models in `ladder` go cheapest-first: `ladder[0]` is the start tier,
/// `ladder.last()` is the ceiling.
pub struct EscalationConfig {
    /// Tier ladder — `[fast, standard, deep]` (or any subset). At least one
    /// model must be present; a single-element ladder degenerates to a
    /// pass-through, matching `escalation_policy = "fixed"` per ADR 0011.
    pub ladder: Vec<Model>,
    /// Maximum escalations *past* the start tier. `2` covers
    /// fast → standard → deep. `0` is equivalent to a fixed policy.
    pub max_escalations: usize,
    /// Confidence threshold. Responses with a numeric `confidence` field
    /// strictly less than this trigger escalation. Default `0.6` per ADR 0011.
    pub confidence_threshold: f32,
    /// Optional schema validator. When set, responses that fail the
    /// validator trigger escalation (`schema_invalid`).
    pub schema_validator: Option<SchemaValidator>,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            ladder: Vec::new(),
            max_escalations: 2,
            confidence_threshold: 0.6,
            schema_validator: None,
        }
    }
}

impl EscalationConfig {
    /// Construct a config with a tier ladder. `max_escalations` defaults to
    /// `ladder.len() - 1`, `confidence_threshold` to `0.6`.
    pub fn new(ladder: Vec<Model>) -> Self {
        let max_escalations = ladder.len().saturating_sub(1);
        Self {
            ladder,
            max_escalations,
            confidence_threshold: 0.6,
            schema_validator: None,
        }
    }

    /// Override the confidence threshold (default `0.6`).
    pub fn confidence_threshold(mut self, t: f32) -> Self {
        self.confidence_threshold = t;
        self
    }

    /// Cap the number of escalations past the start tier.
    pub fn max_escalations(mut self, n: usize) -> Self {
        self.max_escalations = n;
        self
    }

    /// Attach a schema validator. Failing the validator triggers escalation.
    pub fn schema_validator<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.schema_validator = Some(Arc::new(f));
        self
    }
}

/// Why a particular call escalated (or why it didn't).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationReason {
    /// The response failed the configured schema validator.
    SchemaInvalid,
    /// The response's `confidence` field was below the threshold.
    ConfidenceBelow,
    /// The response included `"escalate": true`.
    ExplicitRequest,
    /// The response was accepted as-is — no escalation triggered.
    Accepted,
    /// The ladder was exhausted (or `max_escalations` reached) and the
    /// caller got the ceiling-tier response unchanged.
    CeilingExhausted,
}

/// Decorator that implements [ADR 0011]'s escalation ladder.
///
/// One instance per agent. Holds the inner provider, the tier ladder, and
/// the trigger configuration. Behavior:
///
/// - `complete` runs the ladder: try `ladder[0]`, parse triggers, escalate
///   if any fire and an attempt remains. Returns the latest response with
///   cost summed across attempts and `model_used` set to the actual tier
///   that produced it.
/// - `complete_streamed` and `embed` pass through to the inner provider at
///   the start tier — escalation is meaningful only for parsed structured
///   output.
///
/// [ADR 0011]: ../docs/design/adrs/0011-tiered-model-escalation.md
pub struct EscalatingProvider<P: LlmProvider> {
    inner: P,
    config: EscalationConfig,
    last_attempts: AtomicUsize,
    last_reason: std::sync::Mutex<EscalationReason>,
}

impl<P: LlmProvider> EscalatingProvider<P> {
    /// Build an escalating wrapper around `inner` with `config`.
    ///
    /// Panics if `config.ladder` is empty — an empty ladder has no model
    /// to attempt and is always a configuration bug.
    pub fn new(inner: P, config: EscalationConfig) -> Self {
        assert!(
            !config.ladder.is_empty(),
            "EscalationConfig.ladder must have at least one model"
        );
        Self {
            inner,
            config,
            last_attempts: AtomicUsize::new(0),
            last_reason: std::sync::Mutex::new(EscalationReason::Accepted),
        }
    }

    /// Number of inner-provider attempts the most recent `complete` call
    /// made — `1` for an accepted start-tier response, up to
    /// `config.ladder.len()` if escalation ran the ladder to the ceiling.
    pub fn last_attempts_taken(&self) -> usize {
        self.last_attempts.load(Ordering::Relaxed)
    }

    /// Why the most recent `complete` call resolved as it did.
    pub fn last_reason(&self) -> EscalationReason {
        *self.last_reason.lock().unwrap()
    }

    /// Access the wrapped provider (for layering / introspection).
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// Decide whether the response at this tier should trigger escalation.
    fn classify(&self, text: &str) -> Option<EscalationReason> {
        if let Some(v) = &self.config.schema_validator {
            if !v(text) {
                return Some(EscalationReason::SchemaInvalid);
            }
        }
        match parse_response_signals(text) {
            ResponseSignals {
                escalate_flag: true,
                ..
            } => Some(EscalationReason::ExplicitRequest),
            ResponseSignals {
                confidence: Some(c),
                ..
            } if c < self.config.confidence_threshold => Some(EscalationReason::ConfidenceBelow),
            _ => None,
        }
    }
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for EscalatingProvider<P> {
    async fn complete(
        &self,
        prompt: &PromptStructured,
        _caller_model: &Model,
        options: &CompleteOptions,
    ) -> Result<Completion> {
        // Attempt the ladder. The caller's `_caller_model` is intentionally
        // ignored — the EscalatingProvider owns model selection. See ADR
        // 0011's escalation flow.
        let mut summed_cost = Cost::unknown();
        let mut summed_usage = Usage::default();
        let mut latency_ms: u64 = 0;
        let mut last_result: Option<Completion> = None;
        let mut last_reason = EscalationReason::CeilingExhausted;
        let max_attempts = (self.config.max_escalations + 1).min(self.config.ladder.len());

        for (attempt_idx, model) in self.config.ladder.iter().take(max_attempts).enumerate() {
            let completion = self.inner.complete(prompt, model, options).await?;
            sum_cost(&mut summed_cost, &completion.cost);
            sum_usage(&mut summed_usage, &completion.usage);
            latency_ms = latency_ms.saturating_add(completion.latency_ms);

            let trigger = self.classify(&completion.text);
            last_result = Some(completion);

            match trigger {
                None => {
                    last_reason = EscalationReason::Accepted;
                    self.last_attempts.store(attempt_idx + 1, Ordering::Relaxed);
                    *self.last_reason.lock().unwrap() = last_reason;
                    let mut final_completion = last_result.expect("set above");
                    final_completion.cost = summed_cost;
                    final_completion.usage = summed_usage;
                    final_completion.latency_ms = latency_ms;
                    return Ok(final_completion);
                }
                Some(reason) => {
                    last_reason = reason;
                    // Continue to the next tier unless we've hit our cap.
                }
            }
        }

        // Exhausted: return ceiling response with summed cost. ADR 0011 says
        // the caller still gets *something* — surfacing the final tier's
        // output (with a flag the agent layer can read via `last_reason`).
        self.last_attempts
            .store(max_attempts.max(1), Ordering::Relaxed);
        *self.last_reason.lock().unwrap() = last_reason;
        let mut final_completion =
            last_result.ok_or_else(|| Error::Decode("ladder yielded no attempts".into()))?;
        final_completion.cost = summed_cost;
        final_completion.usage = summed_usage;
        final_completion.latency_ms = latency_ms;
        Ok(final_completion)
    }

    async fn complete_streamed(
        &self,
        prompt: &PromptStructured,
        _caller_model: &Model,
        options: &CompleteOptions,
    ) -> Result<StreamedCompletion> {
        // Escalation requires inspecting a parsed structured response, which
        // a streamed call doesn't have until it terminates. Per ADR 0011,
        // latency-sensitive paths set `escalation_policy = "fixed"` —
        // streaming consumers should configure a single-element ladder, or
        // call the inner provider directly. Pass through at the start tier.
        let start = &self.config.ladder[0];
        self.inner.complete_streamed(prompt, start, options).await
    }

    async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Vec<f32>> {
        self.inner.embed(text, model).await
    }
}

#[derive(Default)]
struct ResponseSignals {
    confidence: Option<f32>,
    escalate_flag: bool,
}

/// Best-effort extraction of `confidence` and `escalate` fields from a JSON
/// response. Non-JSON or schema-mismatched bodies return no signals —
/// `schema_invalid` is the validator's job to detect, not this parser's.
fn parse_response_signals(text: &str) -> ResponseSignals {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return ResponseSignals::default(),
    };
    let confidence = parsed
        .get("confidence")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32);
    let escalate_flag = parsed
        .get("escalate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    ResponseSignals {
        confidence,
        escalate_flag,
    }
}

fn sum_cost(acc: &mut Cost, add: &Cost) {
    acc.input_cents += add.input_cents;
    acc.cache_create_cents += add.cache_create_cents;
    acc.cache_read_cents += add.cache_read_cents;
    acc.output_cents += add.output_cents;
    acc.total_cents += add.total_cents;
}

fn sum_usage(acc: &mut Usage, add: &Usage) {
    acc.input_tokens_total = acc
        .input_tokens_total
        .saturating_add(add.input_tokens_total);
    acc.input_tokens_cached = acc
        .input_tokens_cached
        .saturating_add(add.input_tokens_cached);
    acc.input_tokens_cache_create = acc
        .input_tokens_cache_create
        .saturating_add(add.input_tokens_cache_create);
    acc.output_tokens = acc.output_tokens.saturating_add(add.output_tokens);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelProvider;
    use std::sync::Mutex;

    /// Test provider whose responses are a pre-registered FIFO sequence.
    /// Each `complete` consumes one response. Lets a test exercise the
    /// escalation ladder by registering one response per expected attempt.
    struct SequencedProvider {
        responses: Mutex<Vec<&'static str>>,
        calls: Mutex<Vec<Model>>,
    }

    impl SequencedProvider {
        fn new(responses: Vec<&'static str>) -> Self {
            Self {
                responses: Mutex::new(responses),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Model> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmProvider for SequencedProvider {
        async fn complete(
            &self,
            _prompt: &PromptStructured,
            model: &Model,
            _options: &CompleteOptions,
        ) -> Result<Completion> {
            self.calls.lock().unwrap().push(model.clone());
            let text = {
                let mut q = self.responses.lock().unwrap();
                if q.is_empty() {
                    return Err(Error::Decode("SequencedProvider exhausted".into()));
                }
                q.remove(0).to_string()
            };
            Ok(Completion {
                text,
                usage: Usage {
                    input_tokens_total: 100,
                    output_tokens: 50,
                    ..Default::default()
                },
                cost: Cost {
                    input_cents: 1.0,
                    output_cents: 2.0,
                    total_cents: 3.0,
                    ..Cost::unknown()
                },
                model_used: format!("seq/{}", model.name),
                latency_ms: 10,
            })
        }

        async fn complete_streamed(
            &self,
            _prompt: &PromptStructured,
            _model: &Model,
            _options: &CompleteOptions,
        ) -> Result<StreamedCompletion> {
            unreachable!("not exercised in these tests")
        }

        async fn embed(&self, _text: &str, _model: &EmbeddingModel) -> Result<Vec<f32>> {
            unreachable!("not exercised in these tests")
        }
    }

    fn ladder() -> Vec<Model> {
        vec![
            Model {
                provider: ModelProvider::Anthropic,
                name: "fast".to_string(),
            },
            Model {
                provider: ModelProvider::Anthropic,
                name: "standard".to_string(),
            },
            Model {
                provider: ModelProvider::Anthropic,
                name: "deep".to_string(),
            },
        ]
    }

    fn prompt() -> PromptStructured {
        PromptStructured::dynamic_only("test")
    }

    fn caller_model() -> Model {
        Model {
            provider: ModelProvider::Anthropic,
            name: "ignored".to_string(),
        }
    }

    /// High-confidence valid response at start tier → no escalation, single attempt.
    #[tokio::test]
    async fn accepts_at_start_tier_without_escalating() {
        let inner = SequencedProvider::new(vec![r#"{"confidence": 0.95}"#]);
        let provider = EscalatingProvider::new(inner, EscalationConfig::new(ladder()));

        let res = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 1);
        assert_eq!(provider.last_reason(), EscalationReason::Accepted);
        assert_eq!(res.model_used, "seq/fast");
        assert_eq!(provider.inner().calls().len(), 1);
    }

    /// `confidence: 0.3` at start tier → escalates to standard, accepts there.
    #[tokio::test]
    async fn low_confidence_escalates_one_step() {
        let inner =
            SequencedProvider::new(vec![r#"{"confidence": 0.3}"#, r#"{"confidence": 0.9}"#]);
        let provider = EscalatingProvider::new(inner, EscalationConfig::new(ladder()));

        let res = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 2);
        assert_eq!(provider.last_reason(), EscalationReason::Accepted);
        assert_eq!(res.model_used, "seq/standard");
        // Cost summed across both attempts.
        assert_eq!(res.cost.total_cents, 6.0);
        assert_eq!(res.usage.input_tokens_total, 200);
        let attempts = provider.inner().calls();
        assert_eq!(attempts[0].name, "fast");
        assert_eq!(attempts[1].name, "standard");
    }

    /// `"escalate": true` in the response triggers escalation even when
    /// confidence is high.
    #[tokio::test]
    async fn explicit_escalate_flag_triggers() {
        let inner = SequencedProvider::new(vec![
            r#"{"confidence": 0.95, "escalate": true}"#,
            r#"{"confidence": 0.99}"#,
        ]);
        let provider = EscalatingProvider::new(inner, EscalationConfig::new(ladder()));

        let _ = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 2);
        assert_eq!(provider.last_reason(), EscalationReason::Accepted);
    }

    /// Schema validator says invalid at fast → escalates; valid at
    /// standard → accepts.
    #[tokio::test]
    async fn schema_invalid_escalates() {
        let inner = SequencedProvider::new(vec!["this is not json", r#"{"confidence": 0.9}"#]);
        let config = EscalationConfig::new(ladder())
            .schema_validator(|text| serde_json::from_str::<Value>(text).is_ok());
        let provider = EscalatingProvider::new(inner, config);

        let _ = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 2);
        assert_eq!(provider.last_reason(), EscalationReason::Accepted);
    }

    /// Every tier returns low-confidence — ladder exhausts, returns the
    /// ceiling response with the `CeilingExhausted` flag.
    #[tokio::test]
    async fn ladder_exhaustion_returns_ceiling_with_flag() {
        let inner = SequencedProvider::new(vec![
            r#"{"confidence": 0.1}"#,
            r#"{"confidence": 0.2}"#,
            r#"{"confidence": 0.3}"#,
        ]);
        let provider = EscalatingProvider::new(inner, EscalationConfig::new(ladder()));

        let res = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 3);
        assert_eq!(provider.last_reason(), EscalationReason::ConfidenceBelow);
        assert_eq!(res.model_used, "seq/deep");
        // All three tiers' costs summed.
        assert_eq!(res.cost.total_cents, 9.0);
    }

    /// `max_escalations = 0` (fixed policy) never escalates even when the
    /// start-tier response would normally trigger.
    #[tokio::test]
    async fn fixed_policy_never_escalates() {
        let inner = SequencedProvider::new(vec![r#"{"confidence": 0.1}"#]);
        let config = EscalationConfig::new(ladder()).max_escalations(0);
        let provider = EscalatingProvider::new(inner, config);

        let res = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 1);
        assert_eq!(provider.last_reason(), EscalationReason::ConfidenceBelow);
        assert_eq!(res.model_used, "seq/fast");
        assert_eq!(res.cost.total_cents, 3.0);
    }

    /// `max_escalations = 1` allows one escalation but no more even if the
    /// second-tier response also signals it should escalate.
    #[tokio::test]
    async fn max_escalations_cap_is_respected() {
        let inner =
            SequencedProvider::new(vec![r#"{"confidence": 0.1}"#, r#"{"confidence": 0.2}"#]);
        let config = EscalationConfig::new(ladder()).max_escalations(1);
        let provider = EscalatingProvider::new(inner, config);

        let res = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 2);
        assert_eq!(res.model_used, "seq/standard");
    }

    /// Confidence threshold override — 0.8 means 0.7 escalates where 0.6 would not.
    #[tokio::test]
    async fn confidence_threshold_override() {
        let inner =
            SequencedProvider::new(vec![r#"{"confidence": 0.7}"#, r#"{"confidence": 0.9}"#]);
        let config = EscalationConfig::new(ladder()).confidence_threshold(0.8);
        let provider = EscalatingProvider::new(inner, config);

        let _ = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 2);
    }

    /// A response with no `confidence` field is accepted (no signals = no
    /// trigger) — matches ADR 0011's "the confidence trigger only fires
    /// when the agent self-reports".
    #[tokio::test]
    async fn missing_confidence_field_does_not_escalate() {
        let inner = SequencedProvider::new(vec![r#"{"value": 42}"#]);
        let provider = EscalatingProvider::new(inner, EscalationConfig::new(ladder()));

        let _ = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 1);
        assert_eq!(provider.last_reason(), EscalationReason::Accepted);
    }

    /// Single-element ladder is the canonical "fixed" configuration — no
    /// possible escalation regardless of triggers.
    #[tokio::test]
    async fn single_element_ladder_is_pass_through() {
        let inner = SequencedProvider::new(vec![r#"{"confidence": 0.1, "escalate": true}"#]);
        let config = EscalationConfig::new(vec![ladder()[1].clone()]); // standard only
        let provider = EscalatingProvider::new(inner, config);

        let res = provider
            .complete(&prompt(), &caller_model(), &CompleteOptions::default())
            .await
            .unwrap();

        assert_eq!(provider.last_attempts_taken(), 1);
        assert_eq!(res.model_used, "seq/standard");
    }

    #[test]
    #[should_panic(expected = "ladder must have at least one model")]
    fn empty_ladder_panics() {
        let inner = SequencedProvider::new(vec![]);
        let _ = EscalatingProvider::new(inner, EscalationConfig::new(vec![]));
    }

    #[test]
    fn parse_signals_handles_garbage() {
        let s = parse_response_signals("not json at all");
        assert!(s.confidence.is_none());
        assert!(!s.escalate_flag);
    }

    #[test]
    fn parse_signals_partial_object() {
        let s = parse_response_signals(r#"{"confidence": 0.5}"#);
        assert_eq!(s.confidence, Some(0.5));
        assert!(!s.escalate_flag);
    }
}
