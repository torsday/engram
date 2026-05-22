//! LanceDB vector store — first slice of #10 (ADR 0014).
//!
//! Public surface:
//!
//! - [`LanceStore::open`] opens (or creates) a per-model dataset under
//!   `<vault_root>/vectors/`.
//! - [`LanceStore::upsert`] writes one or more vectors. Pre-existing rows
//!   with the same `id` are deleted first so an upsert is delete + insert
//!   in one logical step.
//! - [`LanceStore::ann_query`] runs a top-K ANN search (cosine distance)
//!   and returns `Vec<AnnHit>`.
//! - [`LanceStore::delete`] removes a vector by `id`.
//!
//! # Scope of this slice
//!
//! This PR ships the minimum-viable schema (id / content_hash / embedding)
//! plus open / upsert / ann_query (no filters) / delete. The following are
//! deferred to follow-ups per the issue's AC:
//!
//! - HNSW auto-build threshold (>= 1000 vectors)
//! - `AnnFilter` predicates (note_type, tags, date)
//! - Multi-model coexistence policy beyond `notes_<model>_v1` naming
//! - `garbage_collect_old_versions`
//! - Performance benchmark against the `docs/design/10-performance-budgets.md`
//!   p95 targets
//!
//! Each deferral is filed as its own follow-up with concrete acceptance
//! criteria so the next slice has clear scope.

use std::path::Path;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::Float32Type;
use arrow_array::{
    FixedSizeListArray, RecordBatch, RecordBatchIterator, RecordBatchReader, StringArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use engram_llm::EmbeddingModel;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, DistanceType, Table};
use thiserror::Error;

/// Errors returned by the vector store.
#[derive(Debug, Error)]
pub enum Error {
    /// Underlying LanceDB error.
    #[error("lancedb: {0}")]
    Lance(#[from] lancedb::Error),

    /// Arrow type construction error.
    #[error("arrow: {0}")]
    Arrow(#[from] arrow_schema::ArrowError),

    /// Filesystem error (creating the vectors directory).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Vault path could not be encoded as UTF-8 for the LanceDB URI.
    #[error("vault path is not valid UTF-8: {0}")]
    NonUtf8VaultPath(std::path::PathBuf),

    /// An upsert / query item had an embedding whose length doesn't match
    /// the table's declared dimension.
    #[error(
        "embedding dimension mismatch: store declares dim={expected}, got {actual} for id={id}"
    )]
    DimensionMismatch {
        /// Expected dimension (from the store).
        expected: usize,
        /// Actual dimension (from the caller).
        actual: usize,
        /// The id of the offending vector.
        id: String,
    },

    /// A `NoteId` (or other identifier) contained an apostrophe or
    /// backslash that the slice-level SQL builder does not escape. Caller
    /// should not pass values like these — the canonical NoteId is a ULID
    /// (alphanumeric only), so this should never fire from production
    /// code; surfaced loudly rather than risking SQL injection.
    #[error(
        "identifier `{0}` contains unsafe characters; expected alphanumeric/underscore/hyphen"
    )]
    UnsafeIdentifier(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// One vector to write to the store.
#[derive(Debug, Clone)]
pub struct VectorUpsert {
    /// Note identifier (ULID in production).
    pub id: String,
    /// Content hash of the embedded text (from the embedding pipeline).
    pub content_hash: String,
    /// The vector itself. `embedding.len()` must equal the table's `dim`.
    pub embedding: Vec<f32>,
}

/// One result row from [`LanceStore::ann_query`].
#[derive(Debug, Clone, PartialEq)]
pub struct AnnHit {
    /// The note's id.
    pub id: String,
    /// Cosine distance (lower = closer).
    pub distance: f32,
}

/// LanceDB-backed vector store, scoped to one embedding model.
pub struct LanceStore {
    table: Table,
    dim: usize,
}

impl LanceStore {
    /// Open (or create) the vector dataset for `model` under
    /// `<vault_root>/vectors/`. Returns a handle that can `upsert` /
    /// `ann_query` / `delete`.
    ///
    /// The dataset name is `notes_<sanitized_model_name>_v1`. Switching
    /// models in the future produces a different dataset, leaving the old
    /// one untouched (per ADR 0014 §Multi-model coexistence).
    pub async fn open(vault_root: &Path, model: &EmbeddingModel) -> Result<Self> {
        let dir = vault_root.join("vectors");
        std::fs::create_dir_all(&dir)?;
        let uri = dir
            .to_str()
            .ok_or_else(|| Error::NonUtf8VaultPath(dir.clone()))?;
        let conn = connect(uri).execute().await?;

        let table_name = format!("notes_{}_v1", sanitize_model_name(&model.name));
        let table = match conn.open_table(&table_name).execute().await {
            Ok(t) => t,
            Err(_) => {
                // Create with an empty initial batch — the schema is fixed
                // by `make_schema`.
                let schema = make_schema(model.dim);
                let empty: Box<dyn RecordBatchReader + Send> =
                    Box::new(RecordBatchIterator::new(std::iter::empty(), schema));
                conn.create_table(&table_name, empty).execute().await?
            }
        };
        Ok(Self {
            table,
            dim: model.dim,
        })
    }

    /// Dimension of vectors this store accepts.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Write `items`. Existing rows with the same `id` are deleted first,
    /// so calling `upsert` with a previously-seen `id` replaces the row.
    ///
    /// Empty input is a no-op.
    pub async fn upsert(&self, items: &[VectorUpsert]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        for item in items {
            if item.embedding.len() != self.dim {
                return Err(Error::DimensionMismatch {
                    expected: self.dim,
                    actual: item.embedding.len(),
                    id: item.id.clone(),
                });
            }
            ensure_safe_identifier(&item.id)?;
        }
        // Delete pass — single SQL via id IN (…).
        let ids = items
            .iter()
            .map(|i| format!("'{}'", i.id))
            .collect::<Vec<_>>()
            .join(",");
        self.table.delete(&format!("id IN ({ids})")).await?;
        // Insert pass.
        let batch = build_batch(items, self.dim)?;
        let schema = batch.schema();
        let iter: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(std::iter::once(Ok(batch)), schema));
        self.table.add(iter).execute().await?;
        Ok(())
    }

    /// Top-K ANN search (cosine distance). Results are ordered by
    /// ascending distance (closest first).
    pub async fn ann_query(&self, query: &[f32], limit: usize) -> Result<Vec<AnnHit>> {
        if query.len() != self.dim {
            return Err(Error::DimensionMismatch {
                expected: self.dim,
                actual: query.len(),
                id: "<query>".to_string(),
            });
        }
        let mut stream = self
            .table
            .vector_search(query)?
            .distance_type(DistanceType::Cosine)
            .limit(limit)
            .execute()
            .await?;
        let mut hits = Vec::new();
        while let Some(batch) = stream.try_next().await? {
            extend_hits(&batch, &mut hits);
        }
        Ok(hits)
    }

    /// Remove the row with `id`. No error if absent.
    pub async fn delete(&self, id: &str) -> Result<()> {
        ensure_safe_identifier(id)?;
        self.table.delete(&format!("id = '{id}'")).await?;
        Ok(())
    }

    /// Number of rows in the table. Useful for tests and for the
    /// HNSW-build threshold check that lands in a follow-up.
    pub async fn count(&self) -> Result<usize> {
        let n = self.table.count_rows(None).await?;
        Ok(n)
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────

fn make_schema(dim: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("content_hash", DataType::Utf8, false),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim as i32,
            ),
            true,
        ),
    ]))
}

fn build_batch(items: &[VectorUpsert], dim: usize) -> Result<RecordBatch> {
    let schema = make_schema(dim);
    let ids = StringArray::from_iter_values(items.iter().map(|i| i.id.as_str()));
    let hashes = StringArray::from_iter_values(items.iter().map(|i| i.content_hash.as_str()));
    let embeddings = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        items
            .iter()
            .map(|i| Some(i.embedding.iter().map(|v| Some(*v)).collect::<Vec<_>>())),
        dim as i32,
    );
    RecordBatch::try_new(
        schema,
        vec![Arc::new(ids), Arc::new(hashes), Arc::new(embeddings)],
    )
    .map_err(Error::Arrow)
}

fn extend_hits(batch: &RecordBatch, hits: &mut Vec<AnnHit>) {
    let id_col = batch
        .column_by_name("id")
        .expect("query result must have id column")
        .as_string::<i32>();
    let dist_col = batch
        .column_by_name("_distance")
        .expect("vector_search adds _distance column")
        .as_primitive::<Float32Type>();
    for i in 0..batch.num_rows() {
        hits.push(AnnHit {
            id: id_col.value(i).to_string(),
            distance: dist_col.value(i),
        });
    }
}

/// Model names go into table names (filesystem-visible). Replace any
/// non-`[A-Za-z0-9_]` character with `_` so `text-embedding-3-small` becomes
/// `text_embedding_3_small`.
fn sanitize_model_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Defensive check on identifiers passed into SQL string-concat. ULIDs and
/// our test ids only use `[A-Za-z0-9_-]`; rejecting everything else
/// eliminates the SQL-injection surface without parameter binding.
fn ensure_safe_identifier(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(Error::UnsafeIdentifier(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_llm::ModelProvider;
    use tempfile::tempdir;

    fn model(dim: usize) -> EmbeddingModel {
        EmbeddingModel {
            provider: ModelProvider::OpenAi,
            name: "test-emb".to_string(),
            dim,
        }
    }

    fn upsert(id: &str, hash: &str, v: Vec<f32>) -> VectorUpsert {
        VectorUpsert {
            id: id.to_string(),
            content_hash: hash.to_string(),
            embedding: v,
        }
    }

    #[test]
    fn sanitize_replaces_punctuation() {
        assert_eq!(
            sanitize_model_name("text-embedding-3-small"),
            "text_embedding_3_small"
        );
        assert_eq!(sanitize_model_name("a.b/c"), "a_b_c");
        assert_eq!(sanitize_model_name("clean_name"), "clean_name");
    }

    #[test]
    fn ensure_safe_identifier_rejects_quotes() {
        assert!(ensure_safe_identifier("'; DROP TABLE notes;--").is_err());
        assert!(ensure_safe_identifier("01H8XGJWBWBAA").is_ok());
        assert!(ensure_safe_identifier("uuid-style-id").is_ok());
        assert!(ensure_safe_identifier("").is_err());
    }

    #[tokio::test]
    async fn open_creates_dataset_on_first_call() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(8)).await.unwrap();
        assert_eq!(store.dim(), 8);
        assert_eq!(store.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn upsert_then_count_matches_input() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        store
            .upsert(&[
                upsert("01-id-a", "h1", vec![0.1, 0.2, 0.3, 0.4]),
                upsert("01-id-b", "h2", vec![0.5, 0.6, 0.7, 0.8]),
            ])
            .await
            .unwrap();
        assert_eq!(store.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn upsert_replaces_existing_row_with_same_id() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        store
            .upsert(&[upsert("01-id-a", "h1", vec![0.1, 0.2, 0.3, 0.4])])
            .await
            .unwrap();
        store
            .upsert(&[upsert("01-id-a", "h2", vec![0.9, 0.9, 0.9, 0.9])])
            .await
            .unwrap();
        assert_eq!(
            store.count().await.unwrap(),
            1,
            "second upsert should replace, not duplicate"
        );
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        store
            .upsert(&[
                upsert("01-id-a", "h1", vec![0.1, 0.2, 0.3, 0.4]),
                upsert("01-id-b", "h2", vec![0.5, 0.6, 0.7, 0.8]),
            ])
            .await
            .unwrap();
        store.delete("01-id-a").await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);
        // Delete of a missing id is a no-op (not an error).
        store.delete("01-id-a").await.unwrap();
        assert_eq!(store.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn ann_query_returns_nearest_first() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        // Three vectors at varying distances from query.
        store
            .upsert(&[
                upsert("near", "h-near", vec![1.0, 0.0, 0.0, 0.0]),
                upsert("mid", "h-mid", vec![0.7, 0.7, 0.0, 0.0]),
                upsert("far", "h-far", vec![0.0, 0.0, 0.0, 1.0]),
            ])
            .await
            .unwrap();

        let hits = store.ann_query(&[1.0, 0.0, 0.0, 0.0], 3).await.unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].id, "near", "closest by cosine should be `near`");
        // Distances are non-decreasing.
        for w in hits.windows(2) {
            assert!(
                w[0].distance <= w[1].distance + 1e-6,
                "results must be sorted by distance ascending: {:?}",
                hits
            );
        }
    }

    #[tokio::test]
    async fn ann_query_respects_limit() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        let items: Vec<VectorUpsert> = (0..10)
            .map(|i| {
                let f = i as f32 / 10.0;
                upsert(&format!("id-{i:02}"), "h", vec![f, f, f, f])
            })
            .collect();
        store.upsert(&items).await.unwrap();
        let hits = store.ann_query(&[0.5, 0.5, 0.5, 0.5], 3).await.unwrap();
        assert_eq!(hits.len(), 3, "limit must cap result count");
    }

    #[tokio::test]
    async fn dimension_mismatch_on_upsert_surfaces_error() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        let err = store
            .upsert(&[upsert("01-id-x", "h", vec![1.0, 2.0, 3.0])]) // dim 3, store wants 4
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            Error::DimensionMismatch {
                expected: 4,
                actual: 3,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn dimension_mismatch_on_query_surfaces_error() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        let err = store.ann_query(&[1.0, 2.0, 3.0], 1).await.unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch { .. }));
    }

    #[tokio::test]
    async fn unsafe_identifier_is_rejected_before_sql() {
        let dir = tempdir().unwrap();
        let store = LanceStore::open(dir.path(), &model(4)).await.unwrap();
        let err = store
            .upsert(&[upsert(
                "'; DROP TABLE notes;--",
                "h",
                vec![0.0, 0.0, 0.0, 0.0],
            )])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::UnsafeIdentifier(_)));
    }
}
