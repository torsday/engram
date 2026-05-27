-- Sub-agent invocation attribution per issue #31:
-- when an agent invokes another as a sub-tool, the sub-agent's
-- agent_actions row carries the parent's run_id so the audit trail
-- can join a Curator's sub-Linker writes back to the Curator's run.
--
-- The column is NULLABLE: top-level (human/scheduler-initiated)
-- runs have no parent_run_id and write NULL here. Only sub-agent
-- runs populate it. Foreign key references agent_runs(id) so a
-- deleted run row would cascade-clear the link.

ALTER TABLE agent_actions ADD COLUMN parent_run_id TEXT REFERENCES agent_runs(id);

-- Index for the most common query: "which actions did this run
-- (directly or via sub-agents) produce?" — joins agent_runs by id
-- and agent_actions by parent_run_id.
CREATE INDEX idx_agent_actions_parent_run_id ON agent_actions(parent_run_id)
WHERE parent_run_id IS NOT NULL;
