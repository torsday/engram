-- Migration 001: initial v1 schema
-- All engram v1 tables. Indexes and FTS5 are in 002.

-- Track applied migrations (bootstrapped by the runner before this migration runs).
CREATE TABLE IF NOT EXISTS schema_migrations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT    NOT NULL UNIQUE,
    applied_at  TEXT    NOT NULL,  -- ISO 8601 UTC
    checksum    TEXT    NOT NULL   -- SHA-256 hex of the migration SQL
);

-- ── Core note metadata ────────────────────────────────────────────────────────

-- Note metadata (derived from frontmatter + filesystem).
CREATE TABLE notes (
    id          TEXT PRIMARY KEY,     -- ULID from frontmatter
    path        TEXT NOT NULL UNIQUE, -- relative to vault root
    title       TEXT NOT NULL,
    note_type   TEXT NOT NULL,        -- fleeting, literature, evergreen, moc, archive, journal
    status      TEXT,                 -- draft, candidate-evergreen, evergreen, needs-review, contested
    created_at  TEXT,                 -- ISO 8601 UTC
    modified_at TEXT,                 -- ISO 8601 UTC
    created_by  TEXT,                 -- human or agent name; DERIVED from sidecar provenance
    frontmatter TEXT,                 -- full YAML as JSON for flexible queries
    content     TEXT                  -- raw markdown body (for FTS)
);

-- Link graph (wikilinks resolved to note IDs).
CREATE TABLE links (
    source_id   TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    target_id   TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    context     TEXT,         -- surrounding sentence for display
    created_by  TEXT,         -- DERIVED from inline HTML-comment provenance
    PRIMARY KEY (source_id, target_id)
);

-- Tag index.
CREATE TABLE tags (
    note_id     TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    tag         TEXT NOT NULL,
    PRIMARY KEY (note_id, tag)
);

-- ── Artifact ingestion ────────────────────────────────────────────────────────

CREATE TABLE artifacts (
    hash                TEXT PRIMARY KEY,  -- SHA-256
    filename            TEXT,
    mime_type           TEXT,
    size_bytes          INTEGER,
    source_url          TEXT,
    dropped_at          TEXT,
    classification      TEXT,             -- academic_paper, screenshot, voice_memo, …
    extraction_status   TEXT,             -- received, classified, extracted, drafted, approved
    literature_note_id  TEXT REFERENCES notes(id)
);

-- ── Agent execution ───────────────────────────────────────────────────────────

-- Top-level agent run log (for Watcher).
CREATE TABLE agent_runs (
    id              TEXT PRIMARY KEY,
    agent_name      TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    trigger         TEXT,             -- file_change, cron, on_demand, council
    notes_affected  TEXT,             -- JSON array of note IDs
    outcome         TEXT,             -- auto_land, council_convened, no_action
    deliberation_id TEXT
);

-- Per-write audit trail: every unstaged write by every agent.
CREATE TABLE agent_actions (
    id              TEXT PRIMARY KEY,  -- ULID
    agent_name      TEXT NOT NULL,
    kind            TEXT NOT NULL,     -- link-add, tag-norm, note-create, …
    files           TEXT NOT NULL,     -- JSON array of relative paths
    diff_hash       TEXT NOT NULL,     -- SHA-256 of the patch
    confidence      REAL NOT NULL,     -- 0.0-1.0
    rationale       TEXT NOT NULL,
    deliberation_id TEXT,
    rubric_check    TEXT NOT NULL,     -- pass | fail | n/a
    wrote_at        TEXT NOT NULL,
    human_decision  TEXT,             -- staged | rejected | amended | ignored
    decided_at      TEXT,
    final_diff_hash TEXT,
    git_commit_sha  TEXT
);

-- Outcome tracking (Watcher + Auditor).
CREATE TABLE outcomes (
    id                  TEXT PRIMARY KEY,  -- ULID
    agent_run_id        TEXT NOT NULL REFERENCES agent_runs(id),
    note_id             TEXT REFERENCES notes(id),
    change_kind         TEXT NOT NULL,
    landed_at           TEXT NOT NULL,
    survived_30d        INTEGER,           -- 0/1/NULL
    survived_90d        INTEGER,
    survived_180d       INTEGER,
    visited_after       INTEGER NOT NULL DEFAULT 0,
    linked_after        INTEGER NOT NULL DEFAULT 0,
    modified_by_human   INTEGER NOT NULL DEFAULT 0,
    seeded_note_ids     TEXT,             -- JSON array
    reverted_at         TEXT,
    reversal_reason     TEXT
);

-- Persistent cross-run state per agent.
CREATE TABLE agent_memory (
    agent_name  TEXT NOT NULL,
    key         TEXT NOT NULL,   -- e.g. "rejected:link:noteA:noteB"
    value       TEXT,            -- JSON payload
    created_at  TEXT NOT NULL,
    expires_at  TEXT,            -- NULL = permanent
    PRIMARY KEY (agent_name, key)
);

-- Trust scores (maintained by Watcher).
CREATE TABLE trust_scores (
    agent_name      TEXT PRIMARY KEY,
    total_decisions INTEGER NOT NULL DEFAULT 0,
    accepted        INTEGER NOT NULL DEFAULT 0,
    rejected        INTEGER NOT NULL DEFAULT 0,
    reverted        INTEGER NOT NULL DEFAULT 0,
    acceptance_rate REAL,
    trust_level     TEXT NOT NULL DEFAULT 'medium',
    last_evaluated  TEXT,
    promoted_at     TEXT,
    demotion_reason TEXT
);

-- Optimistic-lock table: prevents simultaneous agent writes to the same note.
CREATE TABLE note_locks (
    note_id     TEXT PRIMARY KEY REFERENCES notes(id) ON DELETE CASCADE,
    locked_by   TEXT NOT NULL,    -- agent_run_id holding the lock
    locked_at   TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);

-- Write-intent registry: agents signal before writing (used for conflict detection).
CREATE TABLE write_intents (
    id          TEXT PRIMARY KEY,  -- ULID
    note_id     TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    agent_name  TEXT NOT NULL,
    intent_kind TEXT NOT NULL,     -- link-add, tag-norm, note-create, …
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);

-- ── Council and deliberation ──────────────────────────────────────────────────

CREATE TABLE deliberations (
    id              TEXT PRIMARY KEY,
    convened_by     TEXT NOT NULL,
    participants    TEXT NOT NULL,    -- JSON array
    outcome         TEXT NOT NULL,    -- land, propose, shelve
    created_at      TEXT NOT NULL,
    transcript_path TEXT,
    session_id      TEXT
);

CREATE TABLE deliberation_votes (
    deliberation_id     TEXT NOT NULL REFERENCES deliberations(id),
    round_number        INTEGER NOT NULL,
    agent_name          TEXT NOT NULL,
    vote                TEXT NOT NULL,   -- approve, request_changes, reject
    rationale           TEXT NOT NULL,
    suggested_edits_path TEXT,
    voted_at            TEXT NOT NULL,
    PRIMARY KEY (deliberation_id, round_number, agent_name)
);

-- Change drafts awaiting human review.
CREATE TABLE proposals (
    id                  TEXT PRIMARY KEY,  -- ULID
    proposing_agent     TEXT NOT NULL,
    proposed_at         TEXT NOT NULL,
    invasiveness        TEXT NOT NULL,     -- mechanical, additive, editorial, structural
    target_note_id      TEXT REFERENCES notes(id),
    proposed_diff_path  TEXT NOT NULL,
    rationale           TEXT NOT NULL,
    confidence          REAL NOT NULL,
    deliberation_id     TEXT,
    status              TEXT NOT NULL,     -- pending, approved, rejected, expired, superseded
    decided_at          TEXT,
    decided_by          TEXT,
    resulting_action_id TEXT REFERENCES agent_actions(id)
);

-- Shelved proposals (council dissent or no defensible critique).
CREATE TABLE shelved (
    id              TEXT PRIMARY KEY,  -- ULID
    proposing_agent TEXT NOT NULL,
    shelved_at      TEXT NOT NULL,
    deliberation_id TEXT NOT NULL,
    reason          TEXT NOT NULL,     -- dissent | no_defensible_critique | timeout | budget
    summary         TEXT NOT NULL,
    transcript_path TEXT NOT NULL
);

-- ── Coordinated flows ─────────────────────────────────────────────────────────

CREATE TABLE flow_runs (
    id                      TEXT PRIMARY KEY,  -- ULID
    flow_kind               TEXT NOT NULL,
    target_id               TEXT,
    started_at              TEXT NOT NULL,
    completed_at            TEXT,
    current_step            INTEGER NOT NULL DEFAULT 0,
    status                  TEXT NOT NULL,     -- running, completed, blocked, failed, abandoned
    blocker_summary         TEXT,
    transcript_path         TEXT NOT NULL,
    estimated_cost_usd      REAL,
    estimated_tokens_min    INTEGER,
    estimated_tokens_max    INTEGER,
    estimator_confidence    REAL,
    user_confirmed_at       TEXT,
    actual_cost_usd         REAL,
    actual_tokens_used      INTEGER,
    midflow_pause_reason    TEXT
);

CREATE TABLE flow_step_results (
    flow_run_id         TEXT NOT NULL REFERENCES flow_runs(id),
    step_number         INTEGER NOT NULL,
    agent_name          TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    completed_at        TEXT,
    outcome             TEXT NOT NULL,     -- success, request_changes, fail, timeout, skipped
    output_path         TEXT,
    error_summary       TEXT,
    estimated_cost_usd  REAL,
    actual_cost_usd     REAL,
    tokens_used         INTEGER,
    PRIMARY KEY (flow_run_id, step_number)
);

-- ── Auditing and prompt evolution ────────────────────────────────────────────

CREATE TABLE audits (
    id              TEXT PRIMARY KEY,
    agent_name      TEXT NOT NULL,
    period          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    samples_read    INTEGER NOT NULL,
    recommendation  TEXT NOT NULL,   -- keep, tune, demote, pause, retire
    rationale_path  TEXT NOT NULL,
    human_decision  TEXT,
    human_decided_at TEXT
);

CREATE TABLE prompt_variants (
    id              TEXT PRIMARY KEY,  -- ULID
    agent_name      TEXT NOT NULL,
    variant_path    TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    status          TEXT NOT NULL,     -- shadow, promoted, rejected, archived
    samples         INTEGER NOT NULL DEFAULT 0,
    delta_acceptance REAL,
    delta_survival  REAL,
    delta_cost      REAL,
    promoted_at     TEXT,
    rejection_reason TEXT
);

-- ── Conversational surfaces ───────────────────────────────────────────────────

CREATE TABLE conversations (
    id          TEXT PRIMARY KEY,
    agent_name  TEXT NOT NULL,
    note_id     TEXT REFERENCES notes(id),
    status      TEXT NOT NULL,     -- active, completed, abandoned
    round       INTEGER NOT NULL DEFAULT 0,
    max_rounds  INTEGER NOT NULL,
    started_at  TEXT NOT NULL,
    completed_at TEXT,
    transcript  TEXT               -- JSON array of turns
);

CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    goal            TEXT NOT NULL,
    status          TEXT NOT NULL,   -- active, paused, completed
    created_at      TEXT NOT NULL,
    completed_at    TEXT,
    config_path     TEXT,
    focus_topics    TEXT,            -- JSON array
    focus_note_ids  TEXT,            -- JSON array
    focus_tags      TEXT             -- JSON array
);

CREATE TABLE dreams (
    id          TEXT PRIMARY KEY,  -- ULID
    agent_name  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    content_path TEXT,
    dream_type  TEXT,              -- analogy, synthesis, bridge, link
    confidence  REAL,
    promoted    INTEGER NOT NULL DEFAULT 0
);

-- ── External MCP clients ──────────────────────────────────────────────────────

CREATE TABLE mcp_clients (
    id              TEXT PRIMARY KEY,  -- ULID
    name            TEXT NOT NULL,
    api_key_hash    TEXT NOT NULL UNIQUE,
    created_at      TEXT NOT NULL,
    last_used_at    TEXT,
    revoked_at      TEXT,
    scopes          TEXT NOT NULL      -- JSON array
);

CREATE TABLE mcp_access_log (
    id               TEXT PRIMARY KEY,  -- ULID
    client_id        TEXT NOT NULL REFERENCES mcp_clients(id),
    called_at        TEXT NOT NULL,
    tool             TEXT NOT NULL,
    args_summary     TEXT,
    response_summary TEXT,
    success          INTEGER NOT NULL,
    error_message    TEXT
);

CREATE TABLE mcp_register_requests (
    id              TEXT PRIMARY KEY,  -- ULID
    name            TEXT NOT NULL,
    purpose         TEXT,
    requested_scopes TEXT NOT NULL,   -- JSON array
    requested_at    TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    status          TEXT NOT NULL,    -- pending, approved, denied, expired
    decided_at      TEXT,
    granted_scopes  TEXT,
    issued_client_id TEXT REFERENCES mcp_clients(id)
);

CREATE TABLE pending_questions (
    id          TEXT PRIMARY KEY,  -- ULID
    client_id   TEXT NOT NULL REFERENCES mcp_clients(id),
    question    TEXT NOT NULL,
    context     TEXT,
    urgency     TEXT NOT NULL DEFAULT 'normal',
    asked_at    TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    status      TEXT NOT NULL,    -- pending, answered, skipped, expired, muted
    answered_at TEXT,
    answer      TEXT,
    user_action TEXT
);

-- ── Corpus digestion ──────────────────────────────────────────────────────────

CREATE TABLE corpus_digestions (
    id              TEXT PRIMARY KEY,  -- ULID
    source_path     TEXT NOT NULL,
    source_slug     TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    status          TEXT NOT NULL,     -- surveying, planned, digesting, completed, paused
    total_notes     INTEGER,
    notes_processed INTEGER NOT NULL DEFAULT 0,
    notes_kept      INTEGER NOT NULL DEFAULT 0,
    notes_discarded INTEGER NOT NULL DEFAULT 0,
    notes_archived  INTEGER NOT NULL DEFAULT 0,
    notes_merged    INTEGER NOT NULL DEFAULT 0,
    policy_path     TEXT
);

CREATE TABLE digestion_items (
    id              TEXT PRIMARY KEY,
    digestion_id    TEXT NOT NULL REFERENCES corpus_digestions(id),
    source_path     TEXT NOT NULL,
    source_hash     TEXT NOT NULL,   -- SHA-256
    cluster_id      TEXT,
    initial_class   TEXT,
    disposition     TEXT,
    engram_note_id  TEXT REFERENCES notes(id),
    batch_id        TEXT,
    status          TEXT NOT NULL DEFAULT 'pending',
    decided_at      TEXT,
    rationale       TEXT
);

CREATE TABLE digestion_clusters (
    id              TEXT PRIMARY KEY,
    digestion_id    TEXT NOT NULL REFERENCES corpus_digestions(id),
    centroid_topic  TEXT,
    note_count      INTEGER NOT NULL,
    proposed_action TEXT,
    synthesis_note_id TEXT REFERENCES notes(id)
);

CREATE TABLE digestion_discards (
    digestion_item_id TEXT NOT NULL REFERENCES digestion_items(id),
    summary           TEXT NOT NULL,
    discarded_at      TEXT NOT NULL,
    PRIMARY KEY (digestion_item_id)
);

-- ── Predictions ───────────────────────────────────────────────────────────────

CREATE TABLE predictions (
    id                  TEXT PRIMARY KEY,  -- ULID
    note_id             TEXT NOT NULL REFERENCES notes(id),
    excerpt             TEXT NOT NULL,
    claimed_at          TEXT NOT NULL,
    due_at              TEXT,
    confidence          REAL,
    topic               TEXT,
    status              TEXT NOT NULL DEFAULT 'pending',
    resolved_at         TEXT,
    resolution_note     TEXT,
    resolution_evidence TEXT
);

-- ── Spaced repetition ─────────────────────────────────────────────────────────

CREATE TABLE flashcards (
    id              TEXT PRIMARY KEY,  -- ULID
    note_id         TEXT NOT NULL REFERENCES notes(id),
    question        TEXT NOT NULL,
    answer          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    stability       REAL,
    difficulty      REAL,
    last_review_at  TEXT,
    next_review_at  TEXT,
    review_count    INTEGER NOT NULL DEFAULT 0,
    lapse_count     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE flashcard_reviews (
    flashcard_id TEXT NOT NULL REFERENCES flashcards(id),
    reviewed_at  TEXT NOT NULL,
    rating       INTEGER NOT NULL,   -- 1=again, 2=hard, 3=good, 4=easy
    PRIMARY KEY (flashcard_id, reviewed_at)
);

-- ── Cost tracking ─────────────────────────────────────────────────────────────

CREATE TABLE token_usage (
    agent_name      TEXT NOT NULL,
    period          TEXT NOT NULL,   -- "2026-04" (monthly)
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    estimated_cost  REAL NOT NULL DEFAULT 0.0,
    landings        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (agent_name, period)
);

CREATE TABLE agent_budgets (
    agent_name          TEXT PRIMARY KEY,
    monthly_token_cap   INTEGER NOT NULL,
    current_period      TEXT NOT NULL,
    paused_for_budget   INTEGER NOT NULL DEFAULT 0,
    paused_at           TEXT,
    paused_reason       TEXT
);

CREATE TABLE token_estimator_calibration (
    agent_name      TEXT NOT NULL,
    period          TEXT NOT NULL,
    calls_observed  INTEGER NOT NULL DEFAULT 0,
    sum_estimated   INTEGER NOT NULL DEFAULT 0,
    sum_actual      INTEGER NOT NULL DEFAULT 0,
    mean_error_pct  REAL,
    multiplier      REAL NOT NULL DEFAULT 1.0,
    PRIMARY KEY (agent_name, period)
);

-- ── Evaluation framework ──────────────────────────────────────────────────────

CREATE TABLE eval_runs (
    id                  TEXT PRIMARY KEY,
    agent               TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    completed_at        TEXT,
    agent_prompt_sha    TEXT NOT NULL,
    agent_config_sha    TEXT NOT NULL,
    model_used          TEXT NOT NULL,
    cases_run           INTEGER NOT NULL,
    cases_passed        INTEGER NOT NULL,
    total_tokens        INTEGER NOT NULL,
    total_cost_usd      REAL NOT NULL,
    aggregate_metrics   TEXT NOT NULL,  -- JSON
    output_path         TEXT NOT NULL
);

CREATE TABLE eval_case_results (
    eval_run_id     TEXT NOT NULL REFERENCES eval_runs(id),
    case_id         TEXT NOT NULL,
    result          TEXT NOT NULL,   -- pass | fail | error
    scores          TEXT NOT NULL,   -- JSON
    failure_reason  TEXT,
    PRIMARY KEY (eval_run_id, case_id)
);

-- ── Embedding cache ───────────────────────────────────────────────────────────

-- Authoritative computed-embedding cache.
-- LanceDB is the queryable ANN index over these vectors (see ADR 0014).
CREATE TABLE embedding_cache (
    content_hash  TEXT NOT NULL,   -- SHA-256 of normalized embeddable text
    model         TEXT NOT NULL,
    model_version TEXT NOT NULL,
    dimensions    INTEGER NOT NULL,
    embedding     BLOB NOT NULL,   -- packed float32 vector
    first_seen_at TEXT NOT NULL,
    last_used_at  TEXT NOT NULL,
    use_count     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (content_hash, model, model_version, dimensions)
);
