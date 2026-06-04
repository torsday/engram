//! Persistence for council deliberations.
//!
//! Two artifacts per deliberation, per `docs/design/03-architecture.md`
//! §Council and deliberation:
//!
//! - the **authoritative transcript** — markdown at
//!   `.engram/deliberations/<id>.md` (human-readable, the source of truth);
//! - the **indexable mirror** — rows in the `deliberations` +
//!   `deliberation_votes` tables (so the council's history is queryable
//!   without parsing markdown).
//!
//! The schema already exists in `migrations/001_initial.sql`; this module is
//! the write-path plus the transcript renderer.
//!
//! ## Why this is DTO-based (no `engram-council` dependency)
//!
//! `engram-council` is the pure decision core; `engram-index` must not depend
//! on it (that would invert the dependency direction — the orchestration layer
//! depends on both). So this module speaks in its own small value types
//! ([`DeliberationRecord`], [`VoteRecord`]). The async driver that owns a live
//! `CouncilSession` (tracked on #317) maps its `Outcome` / `Vote` onto these
//! DTOs before persisting.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

/// The terminal outcome of a deliberation, as stored in
/// `deliberations.outcome` and rendered in the transcript.
///
/// The string forms (`land` / `propose` / `shelve`) are the documented schema
/// vocabulary — they are a stable on-disk contract, so they are spelled out
/// explicitly rather than derived from a Rust enum's `Debug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliberationOutcome {
    /// Converged to an autonomous (unstaged) write.
    Land,
    /// Converged but requires explicit human approval before writing.
    Propose,
    /// Did not converge; stored with dissent annotated.
    Shelve,
}

impl DeliberationOutcome {
    /// The stored string form (`land` / `propose` / `shelve`).
    pub fn as_str(self) -> &'static str {
        match self {
            DeliberationOutcome::Land => "land",
            DeliberationOutcome::Propose => "propose",
            DeliberationOutcome::Shelve => "shelve",
        }
    }
}

/// A single agent's vote in a CRITIQUE round.
///
/// The `as_str` forms match the `deliberation_votes.vote` documented vocabulary
/// (`approve` / `request_changes` / `reject`) — note `request_changes` is
/// snake_case in the schema even though the council enum is `RequestChanges`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteRecordKind {
    /// The proposal is good as-is.
    Approve,
    /// The proposal needs changes before it can land.
    RequestChanges,
    /// The proposal should not land.
    Reject,
}

impl VoteRecordKind {
    /// The stored string form (`approve` / `request_changes` / `reject`).
    pub fn as_str(self) -> &'static str {
        match self {
            VoteRecordKind::Approve => "approve",
            VoteRecordKind::RequestChanges => "request_changes",
            VoteRecordKind::Reject => "reject",
        }
    }
}

/// One row destined for `deliberation_votes`, plus what the transcript needs to
/// render the vote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteRecord {
    /// 1-based round number (1 = initial CRITIQUE, 2 = post-revision).
    pub round_number: u32,
    /// Kebab-case name of the voting agent.
    pub agent_name: String,
    /// How the agent voted.
    pub vote: VoteRecordKind,
    /// One-paragraph rationale.
    pub rationale: String,
    /// Optional relative path to a file holding the agent's suggested diff.
    pub suggested_edits_path: Option<String>,
    /// RFC3339 timestamp the vote was cast (caller-supplied for testability,
    /// matching the `budget_store` convention).
    pub voted_at: String,
}

/// Everything needed to persist one deliberation: the `deliberations` row
/// fields plus the transcript-only fields (proposal text, affected paths,
/// outcome reason) that have no column but belong in the markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliberationRecord {
    /// Unique deliberation id (ULID), the primary key and transcript filename.
    pub id: String,
    /// The agent that convened the council.
    pub convened_by: String,
    /// Participating agents (stored as a JSON array in `participants`).
    pub participants: Vec<String>,
    /// The terminal outcome.
    pub outcome: DeliberationOutcome,
    /// For `Propose` / `Shelve`, the human-readable reason; rendered in the
    /// transcript's Outcome section. `None` for a clean `Land`.
    pub outcome_reason: Option<String>,
    /// RFC3339 timestamp the council convened.
    pub created_at: String,
    /// Optional goal-directed session id.
    pub session_id: Option<String>,
    /// The proposing agent's one-paragraph rationale (transcript body only).
    pub proposal_rationale: String,
    /// Vault-relative paths the proposed change touches (transcript body only).
    pub affected_paths: Vec<String>,
}

/// Persist a deliberation end-to-end: write the authoritative transcript to
/// `<vault_root>/.engram/deliberations/<id>.md`, then record the indexable
/// rows with that transcript path.
///
/// Returns the vault-relative transcript path that was written and stored
/// (`.engram/deliberations/<id>.md`).
///
/// # Errors
///
/// Returns [`PersistError::Io`] if the transcript can't be written, or
/// [`PersistError::Sqlite`] if the row inserts fail. The transcript is written
/// first (it is authoritative); if the DB write then fails, the transcript
/// remains on disk and the error surfaces for the caller to retry the indexing.
pub fn persist(
    conn: &Connection,
    vault_root: &Path,
    record: &DeliberationRecord,
    votes: &[VoteRecord],
) -> Result<String, PersistError> {
    let rel_path = write_transcript(vault_root, record, votes)?;
    record_deliberation(conn, record, votes, &rel_path)?;
    Ok(rel_path)
}

/// Insert the `deliberations` row and all `deliberation_votes` rows in a single
/// transaction. `transcript_path` is the vault-relative path stored in
/// `deliberations.transcript_path`.
///
/// # Errors
///
/// Any SQLite failure rolls back the whole transaction (no partial
/// deliberation is ever left in the index).
pub fn record_deliberation(
    conn: &Connection,
    record: &DeliberationRecord,
    votes: &[VoteRecord],
    transcript_path: &str,
) -> Result<(), PersistError> {
    let participants_json = serde_json::to_string(&record.participants)?;

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO deliberations
            (id, convened_by, participants, outcome, created_at, transcript_path, session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            record.id,
            record.convened_by,
            participants_json,
            record.outcome.as_str(),
            record.created_at,
            transcript_path,
            record.session_id,
        ],
    )?;

    for v in votes {
        tx.execute(
            "INSERT INTO deliberation_votes
                (deliberation_id, round_number, agent_name, vote, rationale, suggested_edits_path, voted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id,
                v.round_number,
                v.agent_name,
                v.vote.as_str(),
                v.rationale,
                v.suggested_edits_path,
                v.voted_at,
            ],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Render the deliberation transcript markdown (pure — no I/O).
///
/// Format (per `01-agents-and-council.md` + #317): YAML frontmatter
/// (`id`, `convened_by`, `participants`, `outcome`, `created`) followed by the
/// proposal, the per-round votes, and the outcome.
pub fn render_transcript(record: &DeliberationRecord, votes: &[VoteRecord]) -> String {
    let mut out = String::new();

    // Frontmatter.
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", record.id));
    out.push_str(&format!("convened_by: {}\n", record.convened_by));
    out.push_str(&format!(
        "participants: [{}]\n",
        record.participants.join(", ")
    ));
    out.push_str(&format!("outcome: {}\n", record.outcome.as_str()));
    out.push_str(&format!("created: {}\n", record.created_at));
    out.push_str("---\n\n");

    // Title + proposal.
    out.push_str(&format!("# Deliberation {}\n\n", record.id));
    out.push_str("## Proposal\n\n");
    out.push_str(&format!("Convened by **{}**.\n\n", record.convened_by));
    out.push_str(&format!("{}\n", record.proposal_rationale.trim_end()));
    if !record.affected_paths.is_empty() {
        out.push_str("\nAffected paths:\n\n");
        for p in &record.affected_paths {
            out.push_str(&format!("- `{p}`\n"));
        }
    }
    out.push('\n');

    // Per-round votes, grouped by round in ascending order.
    let max_round = votes.iter().map(|v| v.round_number).max().unwrap_or(0);
    for round in 1..=max_round {
        let round_votes: Vec<&VoteRecord> =
            votes.iter().filter(|v| v.round_number == round).collect();
        if round_votes.is_empty() {
            continue;
        }
        let suffix = if round == 1 { "" } else { " (post-revision)" };
        out.push_str(&format!("## Round {round} — CRITIQUE{suffix}\n\n"));
        for v in round_votes {
            out.push_str(&format!(
                "- **{}** — {}: {}\n",
                v.agent_name,
                v.vote.as_str(),
                v.rationale.trim()
            ));
        }
        out.push('\n');
    }

    // Outcome.
    out.push_str("## Outcome\n\n");
    let outcome_upper = record.outcome.as_str().to_uppercase();
    match &record.outcome_reason {
        Some(reason) if !reason.is_empty() => {
            out.push_str(&format!("**{outcome_upper}** — {reason}\n"));
        }
        _ => out.push_str(&format!("**{outcome_upper}**\n")),
    }

    out
}

/// Write the transcript to `<vault_root>/.engram/deliberations/<id>.md`,
/// creating the directory if needed. Returns the **vault-relative** path
/// (`.engram/deliberations/<id>.md`) for storage in
/// `deliberations.transcript_path`.
///
/// # Errors
///
/// Propagates any filesystem error (directory creation or file write).
pub fn write_transcript(
    vault_root: &Path,
    record: &DeliberationRecord,
    votes: &[VoteRecord],
) -> std::io::Result<String> {
    let rel = format!(".engram/deliberations/{}.md", record.id);
    let abs: PathBuf = vault_root.join(&rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&abs, render_transcript(record, votes))?;
    Ok(rel)
}

/// Errors from persisting a deliberation.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// Writing the transcript markdown failed.
    #[error("failed to write deliberation transcript: {0}")]
    Io(#[from] std::io::Error),
    /// Serializing the participants list to JSON failed.
    #[error("failed to serialize participants: {0}")]
    Json(#[from] serde_json::Error),
    /// A SQLite insert (or the surrounding transaction) failed.
    #[error("failed to write deliberation rows: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

// ---------------------------------------------------------------------------
// Read-back (used by tests today; the indexer / `trace` tooling later)
// ---------------------------------------------------------------------------

/// A `deliberations` row read back from the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDeliberation {
    /// Deliberation id.
    pub id: String,
    /// Convening agent.
    pub convened_by: String,
    /// Participants (parsed from the stored JSON array).
    pub participants: Vec<String>,
    /// Outcome string (`land` / `propose` / `shelve`).
    pub outcome: String,
    /// RFC3339 created timestamp.
    pub created_at: String,
    /// Vault-relative transcript path.
    pub transcript_path: Option<String>,
    /// Optional session id.
    pub session_id: Option<String>,
}

/// Load a single deliberation row by id. Returns `Ok(None)` if absent.
///
/// # Errors
///
/// Propagates SQLite errors (other than "no row", which maps to `Ok(None)`).
pub fn load_deliberation(
    conn: &Connection,
    id: &str,
) -> Result<Option<StoredDeliberation>, PersistError> {
    let mut stmt = conn.prepare(
        "SELECT id, convened_by, participants, outcome, created_at, transcript_path, session_id
         FROM deliberations WHERE id = ?1",
    )?;
    let mut rows = stmt.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let participants_json: String = row.get(2)?;
    let participants: Vec<String> = serde_json::from_str(&participants_json)?;
    Ok(Some(StoredDeliberation {
        id: row.get(0)?,
        convened_by: row.get(1)?,
        participants,
        outcome: row.get(3)?,
        created_at: row.get(4)?,
        transcript_path: row.get(5)?,
        session_id: row.get(6)?,
    }))
}

/// Count `deliberation_votes` rows for a deliberation. Cheap helper the
/// integration test uses to assert the votes table was populated.
///
/// # Errors
///
/// Propagates SQLite errors.
pub fn count_votes(conn: &Connection, deliberation_id: &str) -> Result<u32, PersistError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM deliberation_votes WHERE deliberation_id = ?1",
        params![deliberation_id],
        |r| r.get(0),
    )?;
    Ok(n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::Migrator;

    /// Fresh in-memory db with all migrations applied (so the real
    /// `deliberations` / `deliberation_votes` schema from 001 is present).
    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        Migrator::new(&conn)
            .apply_all()
            .expect("migrations apply cleanly");
        conn
    }

    fn sample_record() -> DeliberationRecord {
        DeliberationRecord {
            id: "01HXDELIB0000000000000000".into(),
            convened_by: "synthesizer".into(),
            participants: vec![
                "synthesizer".into(),
                "devils-advocate".into(),
                "linker".into(),
            ],
            outcome: DeliberationOutcome::Land,
            outcome_reason: None,
            created_at: "2026-06-03T12:00:00Z".into(),
            session_id: None,
            proposal_rationale: "Name the concept three notes circle around.".into(),
            affected_paths: vec!["concepts/attention.md".into()],
        }
    }

    fn sample_votes() -> Vec<VoteRecord> {
        vec![
            VoteRecord {
                round_number: 1,
                agent_name: "devils-advocate".into(),
                vote: VoteRecordKind::RequestChanges,
                rationale: "Tighten the central claim.".into(),
                suggested_edits_path: None,
                voted_at: "2026-06-03T12:00:10Z".into(),
            },
            VoteRecord {
                round_number: 2,
                agent_name: "devils-advocate".into(),
                vote: VoteRecordKind::Approve,
                rationale: "The revision addresses it.".into(),
                suggested_edits_path: None,
                voted_at: "2026-06-03T12:00:30Z".into(),
            },
        ]
    }

    #[test]
    fn record_then_read_back_round_trips() {
        let conn = fresh_db();
        record_deliberation(
            &conn,
            &sample_record(),
            &sample_votes(),
            ".engram/deliberations/01HXDELIB0000000000000000.md",
        )
        .expect("record");

        let got = load_deliberation(&conn, "01HXDELIB0000000000000000")
            .expect("load")
            .expect("present");
        assert_eq!(got.convened_by, "synthesizer");
        assert_eq!(got.outcome, "land");
        assert_eq!(
            got.participants,
            vec!["synthesizer", "devils-advocate", "linker"]
        );
        assert_eq!(
            got.transcript_path.as_deref(),
            Some(".engram/deliberations/01HXDELIB0000000000000000.md")
        );
        assert_eq!(count_votes(&conn, "01HXDELIB0000000000000000").unwrap(), 2);
    }

    #[test]
    fn vote_strings_match_schema_vocabulary() {
        assert_eq!(VoteRecordKind::Approve.as_str(), "approve");
        assert_eq!(VoteRecordKind::RequestChanges.as_str(), "request_changes");
        assert_eq!(VoteRecordKind::Reject.as_str(), "reject");
        assert_eq!(DeliberationOutcome::Land.as_str(), "land");
        assert_eq!(DeliberationOutcome::Propose.as_str(), "propose");
        assert_eq!(DeliberationOutcome::Shelve.as_str(), "shelve");
    }

    #[test]
    fn load_missing_deliberation_is_none() {
        let conn = fresh_db();
        assert!(load_deliberation(&conn, "nope").unwrap().is_none());
    }

    #[test]
    fn insert_is_atomic_a_bad_vote_rolls_back_the_row() {
        // A vote with a NULL agent_name violates NOT NULL; the whole
        // transaction must roll back, leaving no deliberations row.
        let conn = fresh_db();
        let bad_votes = vec![VoteRecord {
            round_number: 1,
            agent_name: "ok".into(),
            vote: VoteRecordKind::Approve,
            rationale: "fine".into(),
            suggested_edits_path: None,
            voted_at: "2026-06-03T12:00:10Z".into(),
        }];
        // Force a failure: insert the same primary key twice via a duplicate
        // round+agent in two vote rows.
        let mut votes = bad_votes.clone();
        votes.push(bad_votes[0].clone()); // duplicate PK (delib, round, agent)
        let err = record_deliberation(
            &conn,
            &sample_record(),
            &votes,
            ".engram/deliberations/x.md",
        )
        .expect_err("duplicate vote PK must fail");
        assert!(matches!(err, PersistError::Sqlite(_)));
        // Rolled back: no deliberations row survived.
        assert!(load_deliberation(&conn, &sample_record().id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn transcript_renders_documented_format() {
        let md = render_transcript(&sample_record(), &sample_votes());
        // Frontmatter
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: 01HXDELIB0000000000000000\n"));
        assert!(md.contains("convened_by: synthesizer\n"));
        assert!(md.contains("participants: [synthesizer, devils-advocate, linker]\n"));
        assert!(md.contains("outcome: land\n"));
        assert!(md.contains("created: 2026-06-03T12:00:00Z\n"));
        // Body
        assert!(md.contains("## Proposal"));
        assert!(md.contains("Name the concept three notes circle around."));
        assert!(md.contains("- `concepts/attention.md`"));
        assert!(md.contains("## Round 1 — CRITIQUE\n"));
        assert!(md.contains("## Round 2 — CRITIQUE (post-revision)\n"));
        assert!(md.contains("- **devils-advocate** — request_changes: Tighten the central claim."));
        assert!(md.contains("- **devils-advocate** — approve: The revision addresses it."));
        assert!(md.contains("## Outcome\n\n**LAND**"));
    }

    #[test]
    fn shelve_outcome_renders_reason() {
        let mut rec = sample_record();
        rec.outcome = DeliberationOutcome::Shelve;
        rec.outcome_reason = Some("rejected by heretic: duplicates an existing note".into());
        let md = render_transcript(&rec, &sample_votes());
        assert!(md.contains("outcome: shelve\n"));
        assert!(md.contains("**SHELVE** — rejected by heretic: duplicates an existing note"));
    }

    #[test]
    fn persist_writes_transcript_and_rows() {
        let conn = fresh_db();
        let dir = tempfile::tempdir().expect("tempdir");
        let rel = persist(&conn, dir.path(), &sample_record(), &sample_votes()).expect("persist");
        assert_eq!(rel, ".engram/deliberations/01HXDELIB0000000000000000.md");

        // Transcript file exists with the rendered content.
        let abs = dir.path().join(&rel);
        let on_disk = std::fs::read_to_string(&abs).expect("read transcript");
        assert!(on_disk.contains("# Deliberation 01HXDELIB0000000000000000"));

        // Rows present, with the transcript path stored.
        let stored = load_deliberation(&conn, &sample_record().id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.transcript_path.as_deref(), Some(rel.as_str()));
        assert_eq!(count_votes(&conn, &sample_record().id).unwrap(), 2);
    }
}
