//! Per-agent token estimator with monthly calibration.
//!
//! # Design
//!
//! `TokenEstimator` is a trait agents implement (or delegate to
//! `DefaultEstimator`) to predict input/output token ranges before a call.
//! The orchestrator uses these estimates for cost-aware flow planning (ADR 0011).
//!
//! `CalibrationStore` records actual token usage after every call and
//! aggregates it into `token_estimator_calibration` (see migration 001).
//! At the start of each month it recomputes a per-agent multiplier from the
//! previous month's mean error and applies it to future estimates. The
//! multiplier is capped in `[0.5, 2.0]` to prevent runaway drift.

use chrono::Datelike as _;
use rusqlite::{params, Connection};

use crate::types::{Model, ModelProvider, PromptStructured};

// ─── Token estimate ──────────────────────────────────────────────────────────

/// Predicted token consumption for an upcoming LLM call.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenEstimate {
    /// Minimum expected input tokens.
    pub input_tokens_min: u32,
    /// Maximum expected input tokens.
    pub input_tokens_max: u32,
    /// Minimum expected output tokens.
    pub output_tokens_min: u32,
    /// Maximum expected output tokens.
    pub output_tokens_max: u32,
    /// Calibration quality: `1.0` = perfect data, `0.0` = no data.
    pub confidence: f32,
}

impl TokenEstimate {
    /// Mid-point input estimate.
    pub fn input_mid(&self) -> u32 {
        (self.input_tokens_min + self.input_tokens_max) / 2
    }

    /// Mid-point output estimate.
    pub fn output_mid(&self) -> u32 {
        (self.output_tokens_min + self.output_tokens_max) / 2
    }
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// A type that can predict token usage for a structured prompt.
pub trait TokenEstimator: Send + Sync {
    /// Return a token-range estimate for `prompt` on `model`.
    fn estimate(&self, prompt: &PromptStructured, model: &Model) -> TokenEstimate;
}

// ─── Tier-based baseline ─────────────────────────────────────────────────────

/// Rough baseline input+output tokens for a model tier.
/// Real prompts vary; the multiplier calibration tightens this over time.
struct TierBaseline {
    input: u32,
    output: u32,
}

fn tier_baseline(model: &Model) -> TierBaseline {
    let name = model.name.to_lowercase();
    // Haiku-class (fast/cheap)
    if name.contains("haiku") || name.contains("llama3") || name.contains("mistral") {
        TierBaseline {
            input: 750,
            output: 200,
        }
    // Sonnet-class (mid-tier)
    } else if name.contains("sonnet") || name.contains("gpt-4o-mini") {
        TierBaseline {
            input: 1_500,
            output: 400,
        }
    // Opus-class / large (expensive)
    } else if name.contains("opus") || name.contains("gpt-4") {
        TierBaseline {
            input: 3_000,
            output: 800,
        }
    // Ollama / unknown — use mid-tier baseline
    } else {
        TierBaseline {
            input: 1_500,
            output: 400,
        }
    }
}

/// Rough chars-to-tokens ratio (conservative; actual BPE varies).
const CHARS_PER_TOKEN: f32 = 4.0;

/// Ollama has no token-count API — estimate from character count.
const OLLAMA_CHARS_PER_TOKEN: f32 = 4.0;

// ─── DefaultEstimator ────────────────────────────────────────────────────────

/// Stateless estimator using model-tier baselines and prompt length.
///
/// Returns `confidence: 0.0` because it has no calibration data. Use
/// [`CalibrationStore::adjusted_estimate`] to get a calibrated prediction.
#[derive(Debug, Default, Clone)]
pub struct DefaultEstimator;

impl TokenEstimator for DefaultEstimator {
    fn estimate(&self, prompt: &PromptStructured, model: &Model) -> TokenEstimate {
        let baseline = tier_baseline(model);

        let chars_per_tok = if model.provider == ModelProvider::Ollama {
            OLLAMA_CHARS_PER_TOKEN
        } else {
            CHARS_PER_TOKEN
        };

        let head_toks = (prompt.static_head.len() as f32 / chars_per_tok) as u32;
        let tail_toks = (prompt.dynamic_tail.len() as f32 / chars_per_tok) as u32;
        let prompt_toks = head_toks + tail_toks;

        // Use the larger of the baseline or the actual prompt length.
        let input_mid = prompt_toks.max(baseline.input);
        let slack = (input_mid / 4).max(50);

        TokenEstimate {
            input_tokens_min: input_mid.saturating_sub(slack),
            input_tokens_max: input_mid + slack,
            output_tokens_min: baseline.output.saturating_sub(baseline.output / 4),
            output_tokens_max: baseline.output + baseline.output / 4,
            confidence: 0.0,
        }
    }
}

// ─── Estimated cost ──────────────────────────────────────────────────────────

/// Pre-call cost prediction per ADR 0010's cache-hit-rate formula.
#[derive(Debug, Clone, PartialEq)]
pub struct EstimatedCost {
    /// Predicted cost in US cents.
    pub usd_cents: f64,
    /// Mid-point total token estimate (input + output).
    pub tokens: u32,
    /// Calibration confidence from the underlying [`TokenEstimate`].
    pub confidence: f32,
}

// ─── Public estimate_cost API ────────────────────────────────────────────────

/// Estimate the cost of a prompt call before it is issued.
///
/// Uses `recent_cache_hit_rate` to weight cached vs. uncached input costs per
/// ADR 0010. Pass `0.0` when no cache data is available (conservatively bills
/// everything at full input price).
///
/// Returns `None` if the model is not in the price table.
pub fn estimate_cost(
    prompt: &PromptStructured,
    model: &Model,
    recent_cache_hit_rate: f32,
    estimator: &dyn TokenEstimator,
) -> Option<EstimatedCost> {
    use crate::types::Cost;

    let est = estimator.estimate(prompt, model);
    let input_mid = est.input_mid();
    let output_mid = est.output_mid();

    // Build a synthetic Usage split by cache_hit_rate.
    let cached = (input_mid as f32 * recent_cache_hit_rate) as u32;
    let plain = input_mid.saturating_sub(cached);

    let usage = crate::types::Usage {
        input_tokens_total: input_mid,
        input_tokens_cached: cached,
        input_tokens_cache_create: 0,
        output_tokens: output_mid,
    };

    // Borrow the existing Cost machinery — it handles unknown models.
    let cost = Cost::from_usage(&usage, model);

    // Cost::unknown() returns total_cents == 0 for unknown models.
    // Distinguish "zero cost" (Ollama) from "unknown model" via plain_input.
    if cost.total_cents == 0.0 && plain > 0 && model.provider != ModelProvider::Ollama {
        return None;
    }

    Some(EstimatedCost {
        usd_cents: cost.total_cents,
        tokens: input_mid + output_mid,
        confidence: est.confidence,
    })
}

// ─── CalibrationStore ────────────────────────────────────────────────────────

/// Aggregates per-call observations and auto-tunes per-agent multipliers.
///
/// Backed by the `token_estimator_calibration` table (migration 001).
/// Thread-safety: SQLite connections are `!Send`; callers should use a
/// connection per-thread or wrap in a `Mutex`.
pub struct CalibrationStore<'conn> {
    conn: &'conn Connection,
}

impl<'conn> CalibrationStore<'conn> {
    /// Wrap an existing rusqlite connection.
    pub fn new(conn: &'conn Connection) -> Self {
        Self { conn }
    }

    /// Current calendar period key (`"YYYY-MM"`).
    fn current_period() -> String {
        chrono::Utc::now().format("%Y-%m").to_string()
    }

    /// Record one real call's actual token count. Upserts the current month's
    /// row and increments rolling aggregates.
    pub fn record_observation(
        &self,
        agent: &str,
        estimated_tokens: u32,
        actual_tokens: u32,
    ) -> rusqlite::Result<()> {
        let period = Self::current_period();
        self.conn.execute(
            "INSERT INTO token_estimator_calibration
               (agent_name, period, calls_observed, sum_estimated, sum_actual, multiplier)
             VALUES (?1, ?2, 1, ?3, ?4, 1.0)
             ON CONFLICT(agent_name, period) DO UPDATE SET
               calls_observed = calls_observed + 1,
               sum_estimated  = sum_estimated  + ?3,
               sum_actual     = sum_actual     + ?4",
            params![agent, period, estimated_tokens, actual_tokens],
        )?;
        Ok(())
    }

    /// Compute `mean_error_pct` for the given period and store it.
    pub fn finalize_period(&self, agent: &str, period: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE token_estimator_calibration
             SET mean_error_pct = CAST(sum_actual - sum_estimated AS REAL) / NULLIF(sum_estimated, 0)
             WHERE agent_name = ?1 AND period = ?2",
            params![agent, period],
        )?;
        Ok(())
    }

    /// Recompute and persist the multiplier for `agent` based on the previous
    /// month's mean error. Call once at the start of each new month.
    ///
    /// New multiplier = old × (1 + mean_error_pct), clamped to `[0.5, 2.0]`.
    pub fn recompute_multiplier(&self, agent: &str) -> rusqlite::Result<()> {
        // Find the previous month period.
        let now = chrono::Utc::now();
        let prev = if now.month() == 1 {
            format!("{}-12", now.year() - 1)
        } else {
            format!("{}-{:02}", now.year(), now.month() - 1)
        };

        // Finalize previous period first.
        self.finalize_period(agent, &prev)?;

        let result: Option<(f64, f64)> = self
            .conn
            .query_row(
                "SELECT mean_error_pct, multiplier FROM token_estimator_calibration
                 WHERE agent_name = ?1 AND period = ?2",
                params![agent, prev],
                |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
            )
            .ok();

        let Some((mean_error_pct, old_multiplier)) = result else {
            return Ok(());
        };

        let new_multiplier = (old_multiplier * (1.0 + mean_error_pct)).clamp(0.5, 2.0);
        let current = Self::current_period();

        self.conn.execute(
            "INSERT INTO token_estimator_calibration
               (agent_name, period, calls_observed, sum_estimated, sum_actual, multiplier)
             VALUES (?1, ?2, 0, 0, 0, ?3)
             ON CONFLICT(agent_name, period) DO UPDATE SET multiplier = ?3",
            params![agent, current, new_multiplier],
        )?;
        Ok(())
    }

    /// Return the current multiplier for `agent` (defaults to `1.0`).
    pub fn multiplier(&self, agent: &str) -> rusqlite::Result<f64> {
        let period = Self::current_period();
        let m: Option<f64> = self
            .conn
            .query_row(
                "SELECT multiplier FROM token_estimator_calibration
                 WHERE agent_name = ?1 AND period = ?2",
                params![agent, period],
                |row| row.get(0),
            )
            .ok();
        Ok(m.unwrap_or(1.0))
    }

    /// Return a calibration-adjusted estimate for `agent`.
    ///
    /// The estimate from `estimator` is scaled by the current multiplier and
    /// the confidence is derived from the previous month's mean error:
    /// `confidence = 1.0` when `|mean_error_pct| < 0.10`,
    /// `confidence = 0.0` when no calibration data exists or error > 0.30.
    pub fn adjusted_estimate(
        &self,
        agent: &str,
        prompt: &PromptStructured,
        model: &Model,
        estimator: &dyn TokenEstimator,
    ) -> rusqlite::Result<TokenEstimate> {
        let mult = self.multiplier(agent)?;
        let confidence = self.calibration_confidence(agent)?;

        let base = estimator.estimate(prompt, model);
        let scale = |v: u32| -> u32 { (v as f64 * mult).round() as u32 };

        Ok(TokenEstimate {
            input_tokens_min: scale(base.input_tokens_min),
            input_tokens_max: scale(base.input_tokens_max),
            output_tokens_min: scale(base.output_tokens_min),
            output_tokens_max: scale(base.output_tokens_max),
            confidence,
        })
    }

    fn calibration_confidence(&self, agent: &str) -> rusqlite::Result<f32> {
        let now = chrono::Utc::now();
        let prev = if now.month() == 1 {
            format!("{}-12", now.year() - 1)
        } else {
            format!("{}-{:02}", now.year(), now.month() - 1)
        };

        let result: Option<(Option<f64>, i64)> = self
            .conn
            .query_row(
                "SELECT mean_error_pct, calls_observed FROM token_estimator_calibration
                 WHERE agent_name = ?1 AND period = ?2",
                params![agent, prev],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        let Some((Some(mean_error_pct), calls)) = result else {
            return Ok(0.0);
        };

        if calls < 5 {
            return Ok(0.0);
        }

        let abs_err = mean_error_pct.abs() as f32;
        if abs_err < 0.10 {
            Ok(1.0)
        } else if abs_err > 0.30 {
            Ok(0.0)
        } else {
            // Linear interpolation between 1.0 (10%) and 0.0 (30%).
            Ok(1.0 - (abs_err - 0.10) / 0.20)
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Model, PromptStructured};

    fn haiku() -> Model {
        Model::anthropic("claude-3-5-haiku-20241022")
    }

    fn sonnet() -> Model {
        Model::anthropic("claude-sonnet-4-6")
    }

    fn short_prompt() -> PromptStructured {
        PromptStructured::new("System: you are a helper.", "What is 2+2?")
    }

    fn long_prompt() -> PromptStructured {
        let head = "x".repeat(4000);
        let tail = "y".repeat(2000);
        PromptStructured::new(head, tail)
    }

    fn make_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE token_estimator_calibration (
                agent_name      TEXT NOT NULL,
                period          TEXT NOT NULL,
                calls_observed  INTEGER NOT NULL DEFAULT 0,
                sum_estimated   INTEGER NOT NULL DEFAULT 0,
                sum_actual      INTEGER NOT NULL DEFAULT 0,
                mean_error_pct  REAL,
                multiplier      REAL NOT NULL DEFAULT 1.0,
                PRIMARY KEY (agent_name, period)
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn short_prompt_returns_reasonable_bounds() {
        let est = DefaultEstimator;
        let t = est.estimate(&short_prompt(), &haiku());
        assert!(t.input_tokens_min < t.input_tokens_max);
        assert!(t.output_tokens_min < t.output_tokens_max);
        assert!(t.input_tokens_min > 0);
        assert_eq!(t.confidence, 0.0);
    }

    #[test]
    fn long_prompt_exceeds_baseline() {
        let est = DefaultEstimator;
        let t_short = est.estimate(&short_prompt(), &haiku());
        let t_long = est.estimate(&long_prompt(), &haiku());
        assert!(t_long.input_tokens_max > t_short.input_tokens_max);
    }

    #[test]
    fn sonnet_baseline_larger_than_haiku() {
        let est = DefaultEstimator;
        let th = est.estimate(&short_prompt(), &haiku());
        let ts = est.estimate(&short_prompt(), &sonnet());
        assert!(ts.output_tokens_max >= th.output_tokens_max);
    }

    #[test]
    fn multiplier_defaults_to_one() {
        let conn = make_db();
        let store = CalibrationStore::new(&conn);
        let m = store.multiplier("linker").unwrap();
        assert!((m - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn record_and_retrieve_observation() {
        let conn = make_db();
        let store = CalibrationStore::new(&conn);
        store.record_observation("linker", 1000, 1100).unwrap();
        store.record_observation("linker", 1000, 900).unwrap();

        // sum_estimated=2000, sum_actual=2000 — no error.
        let period = CalibrationStore::current_period();
        let (calls, sum_est, sum_act): (i64, i64, i64) = conn
            .query_row(
                "SELECT calls_observed, sum_estimated, sum_actual
                 FROM token_estimator_calibration WHERE agent_name='linker' AND period=?1",
                rusqlite::params![period],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(sum_est, 2000);
        assert_eq!(sum_act, 2000);
        _ = sum_act; // silence lint
    }

    #[test]
    fn adjusted_estimate_scales_with_multiplier() {
        let conn = make_db();
        let store = CalibrationStore::new(&conn);
        let period = CalibrationStore::current_period();

        // Manually set a 1.5 multiplier for the current period.
        conn.execute(
            "INSERT INTO token_estimator_calibration
               (agent_name, period, calls_observed, sum_estimated, sum_actual, multiplier)
             VALUES ('linker', ?1, 10, 1000, 1500, 1.5)",
            rusqlite::params![period],
        )
        .unwrap();

        let est = DefaultEstimator;
        let base = est.estimate(&short_prompt(), &haiku());
        let adj = store
            .adjusted_estimate("linker", &short_prompt(), &haiku(), &est)
            .unwrap();

        let expected_min = (base.input_tokens_min as f64 * 1.5).round() as u32;
        assert_eq!(adj.input_tokens_min, expected_min);
    }

    #[test]
    fn multiplier_clamps_to_range() {
        let conn = make_db();
        // Insert a prior month with 200% overestimate.
        let now = chrono::Utc::now();
        let prev = if now.month() == 1 {
            format!("{}-12", now.year() - 1)
        } else {
            format!("{}-{:02}", now.year(), now.month() - 1)
        };
        conn.execute(
            "INSERT INTO token_estimator_calibration
               (agent_name, period, calls_observed, sum_estimated, sum_actual, mean_error_pct, multiplier)
             VALUES ('linker', ?1, 20, 1000, 3000, 2.0, 1.0)",
            rusqlite::params![prev],
        )
        .unwrap();

        let store = CalibrationStore::new(&conn);
        store.recompute_multiplier("linker").unwrap();

        // Without clamping: 1.0 * (1 + 2.0) = 3.0; clamped to 2.0.
        let m = store.multiplier("linker").unwrap();
        assert!(m <= 2.0, "multiplier={m}");
        assert!(m >= 0.5, "multiplier={m}");
    }

    #[test]
    fn calibration_convergence_over_observations() {
        let conn = make_db();
        let store = CalibrationStore::new(&conn);

        // Simulate 20 calls where actual is always 10% above estimated.
        for _ in 0..20 {
            store.record_observation("scribe", 1000, 1100).unwrap();
        }
        store
            .finalize_period("scribe", &CalibrationStore::current_period())
            .unwrap();

        let period = CalibrationStore::current_period();
        let mean_err: Option<f64> = conn
            .query_row(
                "SELECT mean_error_pct FROM token_estimator_calibration
                 WHERE agent_name='scribe' AND period=?1",
                rusqlite::params![period],
                |r| r.get(0),
            )
            .unwrap();

        let err = mean_err.unwrap();
        // mean_error_pct = (22000 - 20000) / 20000 = 0.1
        assert!((err - 0.10).abs() < 0.01, "mean_error_pct={err}");
    }

    #[test]
    fn estimate_cost_returns_value_for_known_model() {
        let est = DefaultEstimator;
        let prompt = short_prompt();
        let model = haiku();
        let result = estimate_cost(&prompt, &model, 0.5, &est);
        assert!(result.is_some());
        let ec = result.unwrap();
        assert!(ec.usd_cents >= 0.0);
        assert!(ec.tokens > 0);
    }

    #[test]
    fn estimate_cost_cache_hit_rate_lowers_cost() {
        let est = DefaultEstimator;
        let prompt = short_prompt();
        let model = haiku();
        let no_cache = estimate_cost(&prompt, &model, 0.0, &est).unwrap();
        let full_cache = estimate_cost(&prompt, &model, 1.0, &est).unwrap();
        // Cached reads are cheaper, so full_cache cost <= no_cache cost.
        assert!(full_cache.usd_cents <= no_cache.usd_cents);
    }
}
