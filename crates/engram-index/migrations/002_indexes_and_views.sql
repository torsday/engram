-- Migration 002: secondary indexes, FTS5 virtual table, and content-sync triggers.

-- ── FTS5 full-text search ─────────────────────────────────────────────────────

-- Content table: reads body from `notes` automatically.
-- `title` and `content` match columns in the `notes` table.
-- Triggers below keep the index in sync on INSERT / UPDATE / DELETE.
CREATE VIRTUAL TABLE notes_fts USING fts5 (
    title,
    content,
    content = 'notes',
    content_rowid = 'rowid'
);

-- Populate from existing rows (idempotent: INSERT OR IGNORE via trigger firing).
INSERT INTO notes_fts (rowid, title, content)
SELECT rowid, title, content FROM notes;

-- Content-sync triggers.
CREATE TRIGGER notes_ai
    AFTER INSERT ON notes
BEGIN
    INSERT INTO notes_fts (rowid, title, content)
        VALUES (new.rowid, new.title, new.content);
END;

CREATE TRIGGER notes_ad
    AFTER DELETE ON notes
BEGIN
    INSERT INTO notes_fts (notes_fts, rowid, title, content)
        VALUES ('delete', old.rowid, old.title, old.content);
END;

CREATE TRIGGER notes_au
    AFTER UPDATE ON notes
BEGIN
    INSERT INTO notes_fts (notes_fts, rowid, title, content)
        VALUES ('delete', old.rowid, old.title, old.content);
    INSERT INTO notes_fts (rowid, title, content)
        VALUES (new.rowid, new.title, new.content);
END;

-- ── notes ─────────────────────────────────────────────────────────────────────
CREATE INDEX idx_notes_type         ON notes (note_type);
CREATE INDEX idx_notes_status       ON notes (status) WHERE status IS NOT NULL;
CREATE INDEX idx_notes_created_at   ON notes (created_at DESC);
CREATE INDEX idx_notes_modified_at  ON notes (modified_at DESC);
CREATE INDEX idx_notes_created_by   ON notes (created_by) WHERE created_by IS NOT NULL;

-- ── links ─────────────────────────────────────────────────────────────────────
CREATE INDEX idx_links_target       ON links (target_id);
CREATE INDEX idx_links_source       ON links (source_id);

-- ── tags ──────────────────────────────────────────────────────────────────────
CREATE INDEX idx_tags_tag           ON tags (tag);

-- ── agent_actions ─────────────────────────────────────────────────────────────
CREATE INDEX idx_agent_actions_agent    ON agent_actions (agent_name);
CREATE INDEX idx_agent_actions_pending  ON agent_actions (human_decision)
    WHERE human_decision IS NULL;
CREATE INDEX idx_agent_actions_wrote_at ON agent_actions (wrote_at DESC);

-- ── outcomes ──────────────────────────────────────────────────────────────────
CREATE INDEX idx_outcomes_agent_run ON outcomes (agent_run_id);
CREATE INDEX idx_outcomes_note      ON outcomes (note_id);

-- ── agent_memory ──────────────────────────────────────────────────────────────
CREATE INDEX idx_agent_memory_expires ON agent_memory (expires_at)
    WHERE expires_at IS NOT NULL;

-- ── proposals ─────────────────────────────────────────────────────────────────
CREATE INDEX idx_proposals_status   ON proposals (status);
CREATE INDEX idx_proposals_target   ON proposals (target_note_id);

-- ── shelved ───────────────────────────────────────────────────────────────────
CREATE INDEX idx_shelved_at         ON shelved (shelved_at DESC);

-- ── flow_runs ─────────────────────────────────────────────────────────────────
CREATE INDEX idx_flow_runs_status   ON flow_runs (status);

-- ── mcp_access_log ────────────────────────────────────────────────────────────
CREATE INDEX idx_mcp_access_client_time ON mcp_access_log (client_id, called_at);

-- ── mcp_register_requests ────────────────────────────────────────────────────
CREATE INDEX idx_mcp_register_status    ON mcp_register_requests (status);

-- ── pending_questions ─────────────────────────────────────────────────────────
CREATE INDEX idx_pending_questions_status ON pending_questions (client_id, status);

-- ── digestion_items ───────────────────────────────────────────────────────────
CREATE INDEX idx_digestion_items_digestion  ON digestion_items (digestion_id);
CREATE INDEX idx_digestion_items_status     ON digestion_items (digestion_id, status);

-- ── predictions ───────────────────────────────────────────────────────────────
CREATE INDEX idx_predictions_status ON predictions (status);
CREATE INDEX idx_predictions_due    ON predictions (due_at) WHERE due_at IS NOT NULL;

-- ── flashcards ────────────────────────────────────────────────────────────────
CREATE INDEX idx_flashcards_due     ON flashcards (next_review_at);
CREATE INDEX idx_flashcards_note    ON flashcards (note_id);

-- ── eval_runs ─────────────────────────────────────────────────────────────────
CREATE INDEX idx_eval_runs_agent_time ON eval_runs (agent, started_at DESC);

-- ── embedding_cache ───────────────────────────────────────────────────────────
CREATE INDEX idx_embedding_cache_lru ON embedding_cache (last_used_at);

-- ── write_intents ─────────────────────────────────────────────────────────────
CREATE INDEX idx_write_intents_note     ON write_intents (note_id);
CREATE INDEX idx_write_intents_expires  ON write_intents (expires_at);

-- ── note_locks ────────────────────────────────────────────────────────────────
CREATE INDEX idx_note_locks_expires ON note_locks (expires_at);
