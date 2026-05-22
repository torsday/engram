-- Migration 003: atomic_write_sessions table — durable log of in-flight
-- atomic write sessions across the markdown + sidecar + SQLite triple.
--
-- This is distinct from the pre-existing `write_intents` table (migration
-- 001) which is an advisory-lock registry for *note-level conflict
-- detection* (agents signal "I'm about to write this note", expires_at
-- bounds the lock). `atomic_write_sessions` is the WAL-style log for the
-- *durability* protocol — it tracks the lifecycle of a single
-- two-phase write so a crash between rename and SQLite commit is
-- recoverable on restart.
--
-- Each agent write begins with an INSERT here (status = 'begun'). On
-- commit the same row is updated to 'committed' inside the same SQLite
-- transaction that records the note metadata change. On rollback
-- (explicit or via process crash + restart recovery) the row is updated
-- to 'rolled_back'.
--
-- On `engram serve` startup, `atomic_writes::recover_orphaned` scans rows
-- where `status = 'begun'`, finds the matching `.tmp.<intent_id>` files,
-- and either replays the rename (both tmps present → commit) or rolls
-- back (one or both tmps missing).
--
-- See docs/design/03-architecture.md §Atomic writes for the full flow.

CREATE TABLE atomic_write_sessions (
    -- 26-character Crockford-base32 ULID. Caller-provided so the `.tmp`
    -- filename suffix can be computed before the INSERT lands.
    intent_id        TEXT PRIMARY KEY NOT NULL,

    -- Agent that initiated the write. Free-form string (e.g. "linker",
    -- "ingestor"); not a foreign key — agent registry lives elsewhere.
    agent_id         TEXT NOT NULL,

    -- Absolute path to the final markdown file destination. The matching
    -- on-disk tmp is `<target_path>.tmp.<intent_id>` during the begun phase.
    target_path      TEXT NOT NULL,

    -- Absolute path to the final sidecar JSON file destination. The tmp
    -- companion is `<target_sidecar>.tmp.<intent_id>`.
    target_sidecar   TEXT NOT NULL,

    -- SHA-256 hex digest of `<markdown_content>\x00<sidecar_json_canonical>`
    -- — caller-computed before begin(). Used by recovery / audit tools
    -- to detect a stale replay on top of newer content.
    expected_diff_hash TEXT NOT NULL,

    -- Lifecycle: 'begun' → ('committed' | 'rolled_back'). Enforced by a
    -- CHECK constraint so a typo at the call site doesn't silently leave
    -- rows in a state the recovery scan won't recognise.
    status           TEXT NOT NULL CHECK (status IN ('begun', 'committed', 'rolled_back')),

    -- Wall-clock timestamps (ISO 8601 UTC). Useful for diagnostics
    -- ("intents older than 1h stuck in begun") and for ordering recovery.
    started_at       TEXT NOT NULL,
    committed_at     TEXT
);

-- Recovery scans `WHERE status = 'begun'` on startup. Keep that path indexed.
CREATE INDEX idx_atomic_write_sessions_status_started
    ON atomic_write_sessions (status, started_at);
