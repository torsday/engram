//! Budget domain types: per-agent status and system-wide cost summaries.
//!
//! These types are pure data — no SQLite, no I/O. The `engram-index` crate
//! owns the persistence layer (`budget_store`); this crate owns the domain
//! model so other crates can reason about budgets without depending on SQLite.

/// A single agent's budget status for the current period.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentBudgetStatus {
    pub agent_name: String,
    pub monthly_token_cap: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
    pub paused_for_budget: bool,
}

/// System-wide cost summary for the current period.
#[derive(Debug, Clone, PartialEq)]
pub struct CostSummary {
    /// Billing period in `YYYY-MM` format, e.g. `"2026-05"`.
    pub period: String,
    /// Total estimated spend across all agents this period (USD).
    pub total_usd: f64,
    /// Configured monthly hard cap (USD).
    pub monthly_cap_usd: f64,
    /// Warning threshold fraction (e.g. `0.75`). When
    /// `total_usd / monthly_cap_usd >= warning_threshold`, `at_warning` is
    /// set so callers can emit a heads-up before the cap is hit.
    pub warning_threshold: f64,
    /// `total_usd / monthly_cap_usd * 100`. Capped at `100.0` when over.
    pub percent_consumed: f64,
    /// `true` when spend is at or above the warning threshold.
    pub at_warning: bool,
    /// `true` when spend has reached or exceeded the monthly cap.
    pub at_cap: bool,
    /// Per-agent breakdown for the current period.
    pub per_agent: Vec<AgentBudgetStatus>,
}

impl CostSummary {
    /// Build a [`CostSummary`] from pre-queried data and the configured cap.
    ///
    /// Derived fields (`percent_consumed`, `at_warning`, `at_cap`) are
    /// computed here so callers never have to duplicate the logic.
    pub fn new(
        period: impl Into<String>,
        total_usd: f64,
        monthly_cap_usd: f64,
        warning_threshold: f64,
        per_agent: Vec<AgentBudgetStatus>,
    ) -> Self {
        let ratio = if monthly_cap_usd > 0.0 {
            total_usd / monthly_cap_usd
        } else {
            0.0
        };
        let percent_consumed = (ratio * 100.0).min(100.0);
        let at_warning = ratio >= warning_threshold;
        let at_cap = ratio >= 1.0;
        Self {
            period: period.into(),
            total_usd,
            monthly_cap_usd,
            warning_threshold,
            percent_consumed,
            at_warning,
            at_cap,
            per_agent,
        }
    }

    /// `true` if the named agent is currently paused for budget OR if the
    /// system-wide cap has been reached (which pauses all agents implicitly).
    pub fn agent_paused(&self, agent_name: &str) -> bool {
        if self.at_cap {
            return true;
        }
        self.per_agent
            .iter()
            .find(|a| a.agent_name == agent_name)
            .map(|a| a.paused_for_budget)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(name: &str, paused: bool) -> AgentBudgetStatus {
        AgentBudgetStatus {
            agent_name: name.to_string(),
            monthly_token_cap: 100_000,
            input_tokens: 1_000,
            output_tokens: 500,
            estimated_cost_usd: 0.50,
            paused_for_budget: paused,
        }
    }

    #[test]
    fn cost_summary_new_computes_derived_fields() {
        let summary = CostSummary::new("2026-05", 10.0, 50.0, 0.75, vec![]);
        assert_eq!(summary.period, "2026-05");
        assert_eq!(summary.total_usd, 10.0);
        assert_eq!(summary.monthly_cap_usd, 50.0);
        assert!((summary.percent_consumed - 20.0).abs() < 1e-9);
        assert!(!summary.at_warning);
        assert!(!summary.at_cap);
    }

    #[test]
    fn at_warning_triggers_at_threshold() {
        // Exactly at threshold
        let at = CostSummary::new("2026-05", 37.5, 50.0, 0.75, vec![]);
        assert!(at.at_warning);
        assert!(!at.at_cap);

        // Just below threshold
        let below = CostSummary::new("2026-05", 37.4, 50.0, 0.75, vec![]);
        assert!(!below.at_warning);
    }

    #[test]
    fn at_cap_triggers_at_or_above_full_spend() {
        let at = CostSummary::new("2026-05", 50.0, 50.0, 0.75, vec![]);
        assert!(at.at_cap);
        assert!(at.at_warning);
        assert!((at.percent_consumed - 100.0).abs() < 1e-9);

        let over = CostSummary::new("2026-05", 60.0, 50.0, 0.75, vec![]);
        assert!(over.at_cap);
        // percent_consumed is clamped to 100.0
        assert!((over.percent_consumed - 100.0).abs() < 1e-9);
    }

    #[test]
    fn percent_consumed_math() {
        let s = CostSummary::new("2026-05", 25.0, 100.0, 0.75, vec![]);
        assert!((s.percent_consumed - 25.0).abs() < 1e-9);
    }

    #[test]
    fn agent_paused_returns_true_for_individually_paused_agent() {
        let agents = vec![make_agent("linker", true), make_agent("gardener", false)];
        let s = CostSummary::new("2026-05", 10.0, 50.0, 0.75, agents);
        assert!(s.agent_paused("linker"));
        assert!(!s.agent_paused("gardener"));
    }

    #[test]
    fn agent_paused_returns_true_for_all_when_at_cap() {
        let agents = vec![make_agent("linker", false), make_agent("gardener", false)];
        let s = CostSummary::new("2026-05", 50.0, 50.0, 0.75, agents);
        assert!(s.at_cap);
        assert!(s.agent_paused("linker"));
        assert!(s.agent_paused("gardener"));
    }

    #[test]
    fn agent_paused_returns_false_for_unknown_agent_below_cap() {
        let s = CostSummary::new("2026-05", 5.0, 50.0, 0.75, vec![]);
        assert!(!s.agent_paused("nonexistent"));
    }
}
