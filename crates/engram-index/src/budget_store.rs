//! SQLite queries for token usage tracking, agent budget enforcement, and
//! cost summaries. Backed by the `token_usage` and `agent_budgets` tables
//! from `migrations/001_initial.sql`.

use engram_core::budget::{AgentBudgetStatus, CostSummary};
use engram_core::config::CostConfig;
use rusqlite::{params, Connection};

/// Record token usage for an agent after a successful LLM call.
///
/// Uses an upsert so repeated calls accumulate rather than overwrite. The
/// `landed` flag increments the `landings` counter only when the agent
/// actually wrote output (vs. a dry-run or proposal-only call).
pub fn record_token_usage(
    conn: &Connection,
    agent_name: &str,
    period: &str,
    input_tokens: u64,
    output_tokens: u64,
    estimated_cost_usd: f64,
    landed: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO token_usage (agent_name, period, input_tokens, output_tokens, estimated_cost, landings)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(agent_name, period) DO UPDATE SET
           input_tokens  = token_usage.input_tokens  + excluded.input_tokens,
           output_tokens = token_usage.output_tokens + excluded.output_tokens,
           estimated_cost = token_usage.estimated_cost + excluded.estimated_cost,
           landings      = token_usage.landings      + excluded.landings",
        params![
            agent_name,
            period,
            input_tokens as i64,
            output_tokens as i64,
            estimated_cost_usd,
            landed as i64
        ],
    )?;
    Ok(())
}

/// Pause an agent for a budget breach, recording the reason and timestamp.
///
/// If the agent row does not yet exist in `agent_budgets` this is a no-op —
/// the row must have been created by the daemon on first use. Callers that
/// need auto-create semantics should call [`ensure_agent_budget`] first.
pub fn pause_agent_for_budget(
    conn: &Connection,
    agent_name: &str,
    period: &str,
    reason: &str,
) -> rusqlite::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO agent_budgets (agent_name, monthly_token_cap, current_period, paused_for_budget, paused_at, paused_reason)
         VALUES (?1, 0, ?2, 1, ?3, ?4)
         ON CONFLICT(agent_name) DO UPDATE SET
           paused_for_budget = 1,
           current_period    = excluded.current_period,
           paused_at         = excluded.paused_at,
           paused_reason     = excluded.paused_reason",
        params![agent_name, period, now, reason],
    )?;
    Ok(())
}

/// Resume all agents whose `current_period` differs from `current_period`.
///
/// Call this at daemon start (or on the first request of a new month) so
/// agents paused in a prior billing period are automatically unblocked.
/// Returns the number of rows updated.
pub fn resume_agents_for_new_period(
    conn: &Connection,
    current_period: &str,
) -> rusqlite::Result<usize> {
    let rows = conn.execute(
        "UPDATE agent_budgets
            SET paused_for_budget = 0,
                paused_at         = NULL,
                paused_reason     = NULL,
                current_period    = ?1
          WHERE paused_for_budget = 1
            AND current_period   != ?1",
        params![current_period],
    )?;
    Ok(rows)
}

/// Build a [`CostSummary`] for `period` from `token_usage` and `agent_budgets`.
///
/// Agents that have no rows in `token_usage` for `period` are omitted from
/// `per_agent`. The system total is the sum of all `estimated_cost` rows for
/// the given period regardless of whether an `agent_budgets` row exists.
pub fn query_cost_summary(
    conn: &Connection,
    period: &str,
    config: &CostConfig,
) -> rusqlite::Result<CostSummary> {
    // System total for the period.
    let total_usd: f64 = conn.query_row(
        "SELECT COALESCE(SUM(estimated_cost), 0.0) FROM token_usage WHERE period = ?1",
        params![period],
        |row| row.get(0),
    )?;

    // Per-agent breakdown joined with budget pause status.
    let mut stmt = conn.prepare(
        "SELECT
             tu.agent_name,
             tu.input_tokens,
             tu.output_tokens,
             tu.estimated_cost,
             COALESCE(ab.monthly_token_cap, 0)     AS monthly_token_cap,
             COALESCE(ab.paused_for_budget, 0)     AS paused_for_budget
           FROM token_usage tu
           LEFT JOIN agent_budgets ab ON ab.agent_name = tu.agent_name
          WHERE tu.period = ?1
          ORDER BY tu.agent_name",
    )?;

    let per_agent: Vec<AgentBudgetStatus> = stmt
        .query_map(params![period], |row| {
            Ok(AgentBudgetStatus {
                agent_name: row.get(0)?,
                input_tokens: row.get::<_, i64>(1)? as u64,
                output_tokens: row.get::<_, i64>(2)? as u64,
                estimated_cost_usd: row.get(3)?,
                monthly_token_cap: row.get::<_, i64>(4)? as u64,
                paused_for_budget: row.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(CostSummary::new(
        period,
        total_usd,
        config.monthly_usd_cap,
        config.warning_threshold,
        per_agent,
    ))
}

/// Returns `true` if the agent currently has an active budget pause.
pub fn is_agent_paused(conn: &Connection, agent_name: &str) -> rusqlite::Result<bool> {
    let paused: i64 = conn
        .query_row(
            "SELECT COALESCE(paused_for_budget, 0) FROM agent_budgets WHERE agent_name = ?1",
            params![agent_name],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(paused != 0)
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::config::CostConfig;

    /// Seed the tables used by budget_store in an in-memory SQLite db.
    fn setup() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        conn.execute_batch(
            "CREATE TABLE token_usage (
                agent_name TEXT NOT NULL,
                period     TEXT NOT NULL,
                input_tokens  INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_cost REAL    NOT NULL DEFAULT 0.0,
                landings   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (agent_name, period)
            );
            CREATE TABLE agent_budgets (
                agent_name        TEXT PRIMARY KEY,
                monthly_token_cap INTEGER NOT NULL,
                current_period    TEXT NOT NULL,
                paused_for_budget INTEGER NOT NULL DEFAULT 0,
                paused_at         TEXT,
                paused_reason     TEXT
            );",
        )
        .expect("schema setup");
        conn
    }

    #[test]
    fn record_increments_on_conflict() {
        let conn = setup();
        record_token_usage(&conn, "linker", "2026-05", 100, 50, 0.10, true).unwrap();
        record_token_usage(&conn, "linker", "2026-05", 200, 100, 0.20, false).unwrap();

        let (input, output, cost, landings): (i64, i64, f64, i64) = conn
            .query_row(
                "SELECT input_tokens, output_tokens, estimated_cost, landings
                   FROM token_usage WHERE agent_name='linker' AND period='2026-05'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(input, 300);
        assert_eq!(output, 150);
        assert!((cost - 0.30).abs() < 1e-9);
        assert_eq!(landings, 1); // only the first call had landed=true
    }

    #[test]
    fn pause_and_resume_agent() {
        let conn = setup();
        // Seed agent row first.
        conn.execute(
            "INSERT INTO agent_budgets (agent_name, monthly_token_cap, current_period) VALUES ('gardener', 50000, '2026-04')",
            [],
        ).unwrap();

        pause_agent_for_budget(&conn, "gardener", "2026-04", "monthly cap exceeded").unwrap();
        assert!(is_agent_paused(&conn, "gardener").unwrap());

        // New month — should resume.
        let resumed = resume_agents_for_new_period(&conn, "2026-05").unwrap();
        assert_eq!(resumed, 1);
        assert!(!is_agent_paused(&conn, "gardener").unwrap());
    }

    #[test]
    fn resume_does_not_affect_current_period_agents() {
        let conn = setup();
        // Agent paused in current period should NOT be resumed.
        conn.execute(
            "INSERT INTO agent_budgets (agent_name, monthly_token_cap, current_period, paused_for_budget) VALUES ('linker', 50000, '2026-05', 1)",
            [],
        ).unwrap();

        let resumed = resume_agents_for_new_period(&conn, "2026-05").unwrap();
        assert_eq!(resumed, 0);
        assert!(is_agent_paused(&conn, "linker").unwrap());
    }

    #[test]
    fn query_cost_summary_aggregates_correctly() {
        let conn = setup();
        record_token_usage(&conn, "linker", "2026-05", 1000, 500, 1.00, true).unwrap();
        record_token_usage(&conn, "gardener", "2026-05", 2000, 800, 2.50, true).unwrap();

        let cfg = CostConfig {
            monthly_usd_cap: 50.0,
            warning_threshold: 0.75,
            provider_cost_table: "default".into(),
            alert: Default::default(),
        };
        let summary = query_cost_summary(&conn, "2026-05", &cfg).unwrap();
        assert!((summary.total_usd - 3.50).abs() < 1e-9);
        assert_eq!(summary.per_agent.len(), 2);
        assert!(!summary.at_cap);
        assert!(!summary.at_warning);
    }

    #[test]
    fn is_agent_paused_returns_false_for_unknown_agent() {
        let conn = setup();
        assert!(!is_agent_paused(&conn, "nonexistent").unwrap());
    }

    #[test]
    fn pause_upserts_new_agent_row() {
        let conn = setup();
        // No pre-existing row — pause should create it.
        pause_agent_for_budget(&conn, "scribe", "2026-05", "test").unwrap();
        assert!(is_agent_paused(&conn, "scribe").unwrap());
    }
}
