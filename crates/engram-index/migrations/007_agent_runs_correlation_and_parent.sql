-- Persist the runner's two cross-cutting identifiers on agent_runs
-- so consumers can reconstruct an agent's full call chain (parent →
-- sub-agent → sub-sub-agent) via SQL without parsing tracing logs.
--
-- correlation_id: ULID shared across one logical operation. Top-
-- level runs invent one; sub-agent runs inherit the parent's. Index
-- supports "find all runs that share this correlation" queries.
--
-- parent_run_id: FK to agent_runs(id) when this run was invoked via
-- AgentRunner::run_sub_agent. NULL for top-level (human / scheduler
-- / file-change-initiated) runs. Lets the SubAgent integration test
-- (and a future "trace this Curator's full effect" tool) walk the
-- parent chain transitively.

ALTER TABLE agent_runs ADD COLUMN correlation_id TEXT;
ALTER TABLE agent_runs ADD COLUMN parent_run_id TEXT REFERENCES agent_runs(id);

CREATE INDEX idx_agent_runs_correlation_id ON agent_runs(correlation_id)
WHERE correlation_id IS NOT NULL;

CREATE INDEX idx_agent_runs_parent_run_id ON agent_runs(parent_run_id)
WHERE parent_run_id IS NOT NULL;
