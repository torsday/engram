-- Migration 004: agent_runs token + cost + errored columns
--
-- The original agent_runs schema (in 001_initial.sql) carries only the
-- lifecycle fields (started/completed/outcome/notes_affected/deliberation).
-- Per-invocation token usage, dollar cost, and explicit error flagging
-- are needed for:
--
-- - cost-cap enforcement (#38) and per-agent budget tracking
-- - Watcher schema-drift detection (per-tier escalation rates from cost
--   patterns)
-- - eventual `engram status` surfacing of per-agent rolling cost
--
-- All four columns are nullable so historical rows (and rows written
-- before completion) remain valid; the runner populates them in the
-- terminal UPDATE alongside completed_at / outcome.
--
-- `cost_cents` is stored as REAL (matches the float64 Cost type in
-- engram-llm). Sub-cent precision is preserved.
--
-- `errored` is a 0/1 INTEGER (SQLite's bool) for cheap filter queries:
--   SELECT COUNT(*) FROM agent_runs WHERE errored = 1 AND started_at > ?
-- and is set to 1 whenever the run resolved as `outcome = 'errored'`. The
-- redundancy with `outcome` is intentional: filter-on-bool is cheaper
-- than filter-on-string and the duplication is bounded to two columns.

ALTER TABLE agent_runs ADD COLUMN input_tokens INTEGER;
ALTER TABLE agent_runs ADD COLUMN output_tokens INTEGER;
ALTER TABLE agent_runs ADD COLUMN cost_cents REAL;
ALTER TABLE agent_runs ADD COLUMN errored INTEGER NOT NULL DEFAULT 0;
