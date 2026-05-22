//! SQLite ↔ LanceDB reconciliation pass.
//!
//! LanceDB writes are eventually consistent with SQLite per
//! [ADR 0014](../../../../docs/design/adrs/0014-lancedb-vector-storage.md)
//! §Write semantics: a crash between step 7 (SQLite commit) and step 8
//! (async LanceDB upsert) leaves a note that exists in SQLite but has no
//! matching vector. Reconciliation walks the authoritative SQLite `notes`
//! set, compares each note's expected `content_hash` against LanceDB's
//! stored hash, and corrects the divergence by re-embedding and upserting
//! drift/stale entries, deleting orphans.
//!
//! Designed to run synchronously at `engram serve` startup (blocking
//! startup completion) and hourly thereafter; this slice implements the
//! pure `run()` entry point — the daemon plumbing lands when the CLI's
//! `serve` subcommand and a background scheduler exist.
//!
//! # Algorithm
//!
//! 1. Snapshot SQLite: `(id, content)` for every row in `notes`.
//! 2. Compute the expected `content_hash` for each note via the same
//!    [`content_hash`](crate::embeddings::content_hash) ∘
//!    [`normalize_for_hash`](crate::embeddings::normalize_for_hash)
//!    pipeline the embedder uses.
//! 3. Snapshot LanceDB: `(id, content_hash)` for every row, via
//!    [`LanceStore::scan_id_hash`](crate::vector_store::LanceStore::scan_id_hash).
//! 4. Diff:
//!    - SQLite id missing in LanceDB → **missing** (needs embed + upsert).
//!    - SQLite id present, hash mismatch → **stale** (needs re-embed + upsert).
//!    - LanceDB id missing in SQLite → **orphan** (needs delete).
//! 5. Apply corrections, accumulating per-class counts and the run's wall
//!    duration into [`ReconciliationReport`].
//!
//! At 10K notes the in-memory diff runs in a few hundred milliseconds; for
//! 100K+ vaults, a follow-up adds batched scans. Today the full scan is
//! intentional and simple.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use engram_llm::EmbeddingModel;
use rusqlite::Connection;
use thiserror::Error;

use crate::embeddings::{content_hash, normalize_for_hash, EmbeddingPipeline};
use crate::vector_store::{LanceStore, VectorUpsert};

/// Errors returned by [`Reconciler::run`].
#[derive(Debug, Error)]
pub enum Error {
    /// SQLite error while scanning the `notes` table.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// LanceDB error while scanning or applying corrections.
    #[error("vector store: {0}")]
    VectorStore(#[from] crate::vector_store::Error),

    /// Embedding pipeline error during a re-embed.
    #[error("embeddings: {0}")]
    Embeddings(#[from] crate::embeddings::Error),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// What a reconciliation run did.
///
/// Returned by [`Reconciler::run`]. Surfaced to operators via
/// `engram status` (wiring deferred until the CLI command exists).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReconciliationReport {
    /// Rows scanned in SQLite `notes`.
    pub sqlite_notes_scanned: usize,
    /// Rows scanned in LanceDB.
    pub lance_rows_scanned: usize,
    /// SQLite notes whose vector was missing from LanceDB.
    pub missing_in_lance: usize,
    /// SQLite notes whose LanceDB row had a stale `content_hash`.
    pub stale_in_lance: usize,
    /// LanceDB rows whose `id` is no longer present in SQLite.
    pub orphans_in_lance: usize,
    /// Upserts that were queued (missing + stale).
    pub upserts_queued: usize,
    /// Upserts that landed successfully.
    pub upserts_successful: usize,
    /// Upserts that failed during the run (counted; first error returned).
    pub upserts_failed: usize,
    /// Deletes that landed successfully.
    pub deletes_successful: usize,
    /// Wall time the run took, in milliseconds.
    pub duration_ms: u128,
}

impl ReconciliationReport {
    /// `true` when nothing was found to repair — the common steady-state
    /// outcome on a healthy vault.
    pub fn is_clean(&self) -> bool {
        self.missing_in_lance == 0 && self.stale_in_lance == 0 && self.orphans_in_lance == 0
    }
}

/// The reconciler.
///
/// Borrows the SQLite connection, the LanceDB store, the embedding
/// pipeline, and the embedding model to use when re-embedding stale or
/// missing rows.
pub struct Reconciler<'a> {
    sqlite: &'a Mutex<Connection>,
    lance: &'a LanceStore,
    embeddings: &'a EmbeddingPipeline,
    model: &'a EmbeddingModel,
}

impl<'a> Reconciler<'a> {
    /// Construct a reconciler from the three sources of truth and the
    /// embedding model to use for re-embeds.
    ///
    /// The SQLite connection is borrowed under a `Mutex` because rusqlite
    /// `Connection` is `!Sync` — wrap it once at the call site (see the
    /// integration tests for the pattern) and share by reference.
    pub fn new(
        sqlite: &'a Mutex<Connection>,
        lance: &'a LanceStore,
        embeddings: &'a EmbeddingPipeline,
        model: &'a EmbeddingModel,
    ) -> Self {
        Self {
            sqlite,
            lance,
            embeddings,
            model,
        }
    }

    /// Run one reconciliation pass. Returns the report regardless of
    /// per-row upsert/delete failures (those are counted in
    /// `upserts_failed`); only structural errors (SQLite scan, LanceDB
    /// scan) abort the run.
    pub async fn run(&self) -> Result<ReconciliationReport> {
        let started = Instant::now();
        let mut report = ReconciliationReport::default();

        // Snapshot SQLite. The note `content` is the input to the
        // canonical hash pipeline (same pipeline the embedder uses, so
        // the comparison is apples-to-apples).
        let sqlite_rows: Vec<(String, String)> = {
            let conn = self.sqlite.lock().expect("sqlite mutex poisoned");
            let mut stmt = conn.prepare("SELECT id, content FROM notes")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        report.sqlite_notes_scanned = sqlite_rows.len();

        // Expected hash per id.
        let mut expected: HashMap<String, (String, String)> =
            HashMap::with_capacity(sqlite_rows.len());
        for (id, content) in sqlite_rows {
            let normalized = normalize_for_hash(&content);
            let hash = content_hash(&normalized);
            expected.insert(id, (hash, content));
        }

        // Snapshot LanceDB.
        let lance_rows = self.lance.scan_id_hash().await?;
        report.lance_rows_scanned = lance_rows.len();
        let lance_index: HashMap<String, String> = lance_rows.into_iter().collect();

        // Diff: classify every SQLite note and identify orphans.
        let mut to_upsert: Vec<&str> = Vec::new();
        for (id, (expected_hash, _)) in &expected {
            match lance_index.get(id) {
                None => {
                    report.missing_in_lance += 1;
                    to_upsert.push(id);
                }
                Some(actual) if actual != expected_hash => {
                    report.stale_in_lance += 1;
                    to_upsert.push(id);
                }
                Some(_) => {}
            }
        }
        let sqlite_ids: HashSet<&str> = expected.keys().map(String::as_str).collect();
        let orphans: Vec<String> = lance_index
            .keys()
            .filter(|id| !sqlite_ids.contains(id.as_str()))
            .cloned()
            .collect();
        report.orphans_in_lance = orphans.len();
        report.upserts_queued = to_upsert.len();

        // Apply orphans first — independent of embedding work. Failures
        // are surfaced as the first error returned from the run; deletes
        // either succeed or are absent (LanceStore::delete is idempotent).
        for id in &orphans {
            self.lance.delete(id).await?;
            report.deletes_successful += 1;
        }

        // Apply upserts. Each note is re-embedded via the pipeline (which
        // serves cache hits from SQLite for free, so a re-upsert with
        // unchanged content costs nothing beyond the lookup).
        for id in to_upsert {
            let (expected_hash, content) =
                expected.get(id).expect("id was sourced from expected map");
            match self.upsert_one(id, content, expected_hash).await {
                Ok(()) => report.upserts_successful += 1,
                Err(_) => report.upserts_failed += 1,
            }
        }

        report.duration_ms = started.elapsed().as_millis();
        Ok(report)
    }

    /// Re-embed one note via the pipeline and upsert into LanceDB.
    async fn upsert_one(&self, id: &str, content: &str, expected_hash: &str) -> Result<()> {
        let embedding = self.embeddings.embed(content, self.model).await?;
        self.lance
            .upsert(&[VectorUpsert {
                id: id.to_string(),
                content_hash: expected_hash.to_string(),
                embedding: embedding.vector,
            }])
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::Migrator;
    use engram_llm::mock::MockLlmProvider;
    use engram_llm::{EmbeddingModel, ModelProvider};
    use std::sync::Arc;
    use tempfile::TempDir;

    const EMBED_DIM: usize = 4;

    fn model() -> EmbeddingModel {
        EmbeddingModel {
            provider: ModelProvider::OpenAi,
            name: "test-emb".to_string(),
            dim: EMBED_DIM,
        }
    }

    fn mock_provider() -> Arc<MockLlmProvider> {
        Arc::new(MockLlmProvider::builder().mode_echo().build())
    }

    fn setup_sqlite() -> Mutex<Connection> {
        let conn = Connection::open_in_memory().expect("sqlite open");
        Migrator::new(&conn).apply_all().expect("migrations");
        Mutex::new(conn)
    }

    fn insert_note(sqlite: &Mutex<Connection>, id: &str, content: &str) {
        let conn = sqlite.lock().unwrap();
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content) \
             VALUES (?1, ?2, ?3, 'evergreen', ?4)",
            rusqlite::params![id, format!("{id}.md"), id, content],
        )
        .expect("insert note");
    }

    fn update_note_content(sqlite: &Mutex<Connection>, id: &str, content: &str) {
        let conn = sqlite.lock().unwrap();
        conn.execute(
            "UPDATE notes SET content = ?1 WHERE id = ?2",
            rusqlite::params![content, id],
        )
        .expect("update note");
    }

    fn delete_note_row(sqlite: &Mutex<Connection>, id: &str) {
        let conn = sqlite.lock().unwrap();
        conn.execute("DELETE FROM notes WHERE id = ?1", rusqlite::params![id])
            .expect("delete note");
    }

    async fn open_lance(tmpdir: &TempDir) -> LanceStore {
        LanceStore::open(tmpdir.path(), &model())
            .await
            .expect("lance open")
    }

    fn pipeline(sqlite: &Mutex<Connection>) -> EmbeddingPipeline {
        // The pipeline takes ownership of its own connection; for tests
        // we open a second in-memory connection that shares the same
        // schema. Migrations are idempotent.
        let _ = sqlite; // not actually shared — kept for callsite clarity
        let p_conn = Connection::open_in_memory().unwrap();
        Migrator::new(&p_conn).apply_all().unwrap();
        EmbeddingPipeline::new(p_conn, mock_provider())
    }

    /// Helper to embed + upsert directly, simulating the normal write
    /// path (step 8 in the atomic-write flow).
    async fn write_normal(lance: &LanceStore, pipe: &EmbeddingPipeline, id: &str, content: &str) {
        let normalized = normalize_for_hash(content);
        let hash = content_hash(&normalized);
        let emb = pipe.embed(content, &model()).await.expect("embed");
        lance
            .upsert(&[VectorUpsert {
                id: id.to_string(),
                content_hash: hash,
                embedding: emb.vector,
            }])
            .await
            .expect("upsert");
    }

    #[tokio::test]
    async fn clean_vault_reports_no_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let sqlite = setup_sqlite();
        let lance = open_lance(&tmp).await;
        let pipe = pipeline(&sqlite);

        insert_note(&sqlite, "01HK0000000000000000000001", "alpha body");
        insert_note(&sqlite, "01HK0000000000000000000002", "beta body");
        write_normal(&lance, &pipe, "01HK0000000000000000000001", "alpha body").await;
        write_normal(&lance, &pipe, "01HK0000000000000000000002", "beta body").await;

        let report = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run");

        assert!(report.is_clean(), "expected clean run: {report:?}");
        assert_eq!(report.sqlite_notes_scanned, 2);
        assert_eq!(report.lance_rows_scanned, 2);
        assert_eq!(report.upserts_queued, 0);
        assert_eq!(report.deletes_successful, 0);
    }

    #[tokio::test]
    async fn missing_lance_row_is_repaired() {
        // Simulates the crash-between-steps-7-and-8 scenario: SQLite has
        // the note but the async LanceDB upsert never completed.
        let tmp = tempfile::tempdir().unwrap();
        let sqlite = setup_sqlite();
        let lance = open_lance(&tmp).await;
        let pipe = pipeline(&sqlite);

        insert_note(&sqlite, "01HK000000000000000000000A", "orphaned write");

        let report = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run");

        assert_eq!(report.missing_in_lance, 1);
        assert_eq!(report.upserts_queued, 1);
        assert_eq!(report.upserts_successful, 1);
        assert_eq!(report.upserts_failed, 0);

        // Second run is clean.
        let second = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run 2");
        assert!(second.is_clean(), "expected clean post-repair: {second:?}");
        assert_eq!(second.lance_rows_scanned, 1);
    }

    #[tokio::test]
    async fn stale_content_hash_is_repaired() {
        // Note's content was updated in SQLite but the LanceDB upsert for
        // the new content never landed.
        let tmp = tempfile::tempdir().unwrap();
        let sqlite = setup_sqlite();
        let lance = open_lance(&tmp).await;
        let pipe = pipeline(&sqlite);

        insert_note(&sqlite, "01HK000000000000000000000B", "v1 content");
        write_normal(&lance, &pipe, "01HK000000000000000000000B", "v1 content").await;
        update_note_content(&sqlite, "01HK000000000000000000000B", "v2 content");

        let report = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run");

        assert_eq!(report.stale_in_lance, 1);
        assert_eq!(report.missing_in_lance, 0);
        assert_eq!(report.upserts_queued, 1);
        assert_eq!(report.upserts_successful, 1);

        // The post-repair hash matches the new content.
        let rows = lance.scan_id_hash().await.unwrap();
        let normalized = normalize_for_hash("v2 content");
        let expected = content_hash(&normalized);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, expected);
    }

    #[tokio::test]
    async fn orphan_lance_row_is_deleted() {
        // Note was removed from SQLite (sync_delete path), but a vector
        // is still in LanceDB.
        let tmp = tempfile::tempdir().unwrap();
        let sqlite = setup_sqlite();
        let lance = open_lance(&tmp).await;
        let pipe = pipeline(&sqlite);

        insert_note(&sqlite, "01HK000000000000000000000C", "to be deleted");
        write_normal(&lance, &pipe, "01HK000000000000000000000C", "to be deleted").await;
        delete_note_row(&sqlite, "01HK000000000000000000000C");

        let report = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run");

        assert_eq!(report.orphans_in_lance, 1);
        assert_eq!(report.deletes_successful, 1);
        assert_eq!(report.missing_in_lance, 0);
        assert_eq!(report.upserts_queued, 0);

        let count = lance.count().await.unwrap();
        assert_eq!(count, 0, "orphan must be deleted");
    }

    #[tokio::test]
    async fn mixed_drift_repairs_each_class() {
        // One healthy row, one missing, one stale, one orphan.
        let tmp = tempfile::tempdir().unwrap();
        let sqlite = setup_sqlite();
        let lance = open_lance(&tmp).await;
        let pipe = pipeline(&sqlite);

        // healthy
        insert_note(&sqlite, "01HK000000000000000000000H", "healthy");
        write_normal(&lance, &pipe, "01HK000000000000000000000H", "healthy").await;
        // missing — in SQLite only
        insert_note(&sqlite, "01HK000000000000000000000M", "missing-side");
        // stale — content updated in SQLite after the vector landed
        insert_note(&sqlite, "01HK000000000000000000000S", "stale-v1");
        write_normal(&lance, &pipe, "01HK000000000000000000000S", "stale-v1").await;
        update_note_content(&sqlite, "01HK000000000000000000000S", "stale-v2");
        // orphan — vector with no SQLite row
        write_normal(&lance, &pipe, "01HK000000000000000000000O", "orphan body").await;

        let report = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run");

        assert_eq!(report.sqlite_notes_scanned, 3);
        assert_eq!(report.missing_in_lance, 1);
        assert_eq!(report.stale_in_lance, 1);
        assert_eq!(report.orphans_in_lance, 1);
        assert_eq!(report.upserts_queued, 2);
        assert_eq!(report.upserts_successful, 2);
        assert_eq!(report.deletes_successful, 1);

        let second = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run 2");
        assert!(second.is_clean(), "expected clean second run: {second:?}");
    }

    #[tokio::test]
    async fn whitespace_only_edits_dont_trigger_reupsert() {
        // ADR 0012: hash is over normalized content. A trailing-space
        // edit should not register as drift.
        let tmp = tempfile::tempdir().unwrap();
        let sqlite = setup_sqlite();
        let lance = open_lance(&tmp).await;
        let pipe = pipeline(&sqlite);

        insert_note(&sqlite, "01HK000000000000000000000W", "clean line");
        write_normal(&lance, &pipe, "01HK000000000000000000000W", "clean line").await;
        // Same content but with trailing whitespace — normalized hash is
        // identical, so reconciliation must not consider this stale.
        update_note_content(&sqlite, "01HK000000000000000000000W", "clean line   ");

        let report = Reconciler::new(&sqlite, &lance, &pipe, &model())
            .run()
            .await
            .expect("run");

        assert!(
            report.is_clean(),
            "cosmetic edit should not drift: {report:?}"
        );
    }
}
