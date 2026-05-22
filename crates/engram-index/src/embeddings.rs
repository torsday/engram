//! Embedding pipeline with content-hash-keyed cache.
//!
//! Per [ADR 0012](../../../../docs/design/adrs/0012-embedding-cache-by-content-hash.md),
//! every embed call goes through this cache before reaching the LLM provider.
//! The cache key is `(content_hash, model, model_version, dimensions)`. A
//! hit is a SQLite point lookup + use_count bump; a miss falls through to
//! the provider and writes the result back.
//!
//! # Hash semantics
//!
//! Hash input is the **normalized** text: trailing whitespace stripped per
//! line, runs of two or more blank lines collapsed to one. This means tiny
//! editor reformat noise (a trailing space added or removed) does not
//! invalidate the cache — the embedding is stable across cosmetic edits.
//!
//! See [`normalize_for_hash`] for the exact rules and [`content_hash`] for
//! the SHA-256 wrapper.
//!
//! # Storage
//!
//! The `embedding_cache` table is created by migration 001. Vectors are
//! stored as packed little-endian float32 BLOBs. `dimensions` is part of
//! the primary key so a model that ships variants at different output
//! sizes (e.g. truncatable embeddings) doesn't collide.
//!
//! # Async dispatch
//!
//! The provider call is async; the SQLite calls are synchronous (rusqlite).
//! SQLite point lookups and inserts at this size are sub-millisecond, so
//! they run on the caller's task without `spawn_blocking`. If profiling
//! later shows the SQLite hold time blocks scheduler quanta, switch the
//! cache operations to `spawn_blocking`.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use engram_llm::{EmbeddingModel, LlmProvider};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// One embedding returned by the pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    /// The vector itself. `vector.len() == dimensions`.
    pub vector: Vec<f32>,
    /// Provider-specific model identifier (`EmbeddingModel::name`).
    pub model: String,
    /// Schema-level model version (see crate-level docs). Today this is
    /// always the pipeline's configured version; future provider-specific
    /// detection can override.
    pub model_version: String,
    /// Dimensionality. Equal to `vector.len()`.
    pub dimensions: usize,
    /// `true` if this came from the SQLite cache; `false` if the provider
    /// was called.
    pub cache_hit: bool,
}

/// Errors returned by [`EmbeddingPipeline`].
#[derive(Debug, Error)]
pub enum Error {
    /// SQLite error reading or writing the cache.
    #[error("embedding cache: {0}")]
    Cache(#[from] rusqlite::Error),

    /// Underlying LLM provider call failed.
    #[error("embedding provider: {0}")]
    Provider(#[from] engram_llm::Error),

    /// Provider returned a vector whose length does not match the model's
    /// declared dimensionality. Surfaced rather than silently storing the
    /// mismatch.
    #[error(
        "embedding dimension mismatch: model `{model}` declares dim={expected}, \
         provider returned {actual}"
    )]
    DimensionMismatch {
        /// Provider model name.
        model: String,
        /// Expected (from `EmbeddingModel::dim`).
        expected: usize,
        /// Actual (from provider).
        actual: usize,
    },
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Embedding pipeline — provider in front, SQLite cache in back.
///
/// Construct with [`EmbeddingPipeline::new`]. The connection is owned by
/// the pipeline; share via `Arc<EmbeddingPipeline>` if multiple call sites
/// need it.
pub struct EmbeddingPipeline {
    conn: Mutex<Connection>,
    provider: Arc<dyn LlmProvider>,
    model_version: String,
}

impl EmbeddingPipeline {
    /// Default `model_version` recorded for cached entries when the caller
    /// does not override it. See [`with_model_version`](Self::with_model_version).
    pub const DEFAULT_MODEL_VERSION: &'static str = "v1";

    /// Build a pipeline backed by `conn` and dispatching to `provider`.
    ///
    /// The caller is responsible for having applied
    /// `migrations/001_initial.sql` to `conn` (via [`crate::sqlite::Migrator`])
    /// before constructing the pipeline.
    pub fn new(conn: Connection, provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            conn: Mutex::new(conn),
            provider,
            model_version: Self::DEFAULT_MODEL_VERSION.to_string(),
        }
    }

    /// Override the `model_version` value written to the cache. Used when
    /// the provider's API surface advances in a way that should invalidate
    /// existing cached entries (e.g. OpenAI bumps the embeddings model
    /// without changing the model name).
    pub fn with_model_version(mut self, version: impl Into<String>) -> Self {
        self.model_version = version.into();
        self
    }

    /// Embed `text` via the cache: hit returns the cached vector and
    /// bumps `use_count` + `last_used_at`; miss calls the provider and
    /// writes the result back.
    pub async fn embed(&self, text: &str, model: &EmbeddingModel) -> Result<Embedding> {
        let normalized = normalize_for_hash(text);
        let hash = content_hash(&normalized);

        // Cache lookup.
        if let Some(vector) = self.lookup(&hash, model)? {
            return Ok(Embedding {
                vector,
                model: model.name.clone(),
                model_version: self.model_version.clone(),
                dimensions: model.dim,
                cache_hit: true,
            });
        }

        // Miss path — call provider, write back.
        self.fetch_and_store(text, &hash, model).await
    }

    /// Embed `text` bypassing the cache entirely. The provider is always
    /// called; the result is not written back. Useful for the
    /// `--no-embedding-cache` debug flag and for benchmarks that want to
    /// measure raw provider latency.
    pub async fn embed_bypassing_cache(
        &self,
        text: &str,
        model: &EmbeddingModel,
    ) -> Result<Embedding> {
        let vector = self.provider.embed(text, model).await?;
        if vector.len() != model.dim {
            return Err(Error::DimensionMismatch {
                model: model.name.clone(),
                expected: model.dim,
                actual: vector.len(),
            });
        }
        Ok(Embedding {
            vector,
            model: model.name.clone(),
            model_version: self.model_version.clone(),
            dimensions: model.dim,
            cache_hit: false,
        })
    }

    fn lookup(&self, hash: &str, model: &EmbeddingModel) -> Result<Option<Vec<f32>>> {
        let conn = self.conn.lock().expect("embedding cache mutex poisoned");
        let now = Utc::now().to_rfc3339();
        let row: Option<Vec<u8>> = conn
            .query_row(
                "SELECT embedding FROM embedding_cache \
                 WHERE content_hash = ?1 AND model = ?2 \
                   AND model_version = ?3 AND dimensions = ?4",
                params![hash, model.name, self.model_version, model.dim as i64],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(blob) = row {
            // Bump LRU + use_count in the same connection.
            conn.execute(
                "UPDATE embedding_cache SET use_count = use_count + 1, last_used_at = ?5 \
                 WHERE content_hash = ?1 AND model = ?2 \
                   AND model_version = ?3 AND dimensions = ?4",
                params![hash, model.name, self.model_version, model.dim as i64, now],
            )?;
            Ok(Some(blob_to_vec(&blob)))
        } else {
            Ok(None)
        }
    }

    async fn fetch_and_store(
        &self,
        text: &str,
        hash: &str,
        model: &EmbeddingModel,
    ) -> Result<Embedding> {
        let vector = self.provider.embed(text, model).await?;
        if vector.len() != model.dim {
            return Err(Error::DimensionMismatch {
                model: model.name.clone(),
                expected: model.dim,
                actual: vector.len(),
            });
        }
        let now = Utc::now().to_rfc3339();
        let blob = vec_to_blob(&vector);
        {
            let conn = self.conn.lock().expect("embedding cache mutex poisoned");
            conn.execute(
                "INSERT INTO embedding_cache \
                   (content_hash, model, model_version, dimensions, \
                    embedding, first_seen_at, last_used_at, use_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1) \
                 ON CONFLICT(content_hash, model, model_version, dimensions) DO UPDATE SET \
                   use_count = use_count + 1, last_used_at = ?6",
                params![
                    hash,
                    model.name,
                    self.model_version,
                    model.dim as i64,
                    blob,
                    now
                ],
            )?;
        }
        Ok(Embedding {
            vector,
            model: model.name.clone(),
            model_version: self.model_version.clone(),
            dimensions: model.dim,
            cache_hit: false,
        })
    }
}

/// Normalize `text` for hashing.
///
/// Rules (in order):
///
/// 1. Trim trailing whitespace from each line (so a stray trailing space
///    doesn't break the cache).
/// 2. Collapse runs of two or more blank lines into a single blank line.
/// 3. Strip leading and trailing whitespace from the whole string.
///
/// The result is what gets fed to SHA-256. The original `text` is what
/// gets sent to the provider — we only normalize for the cache key.
pub fn normalize_for_hash(text: &str) -> String {
    // Step 1: trim trailing whitespace per line.
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.trim_end_matches(['\r', ' ', '\t']))
        .collect();

    // Step 2: collapse runs of blank lines.
    let mut collapsed: Vec<&str> = Vec::with_capacity(lines.len());
    let mut last_blank = false;
    for line in lines {
        let blank = line.is_empty();
        if blank && last_blank {
            continue;
        }
        collapsed.push(line);
        last_blank = blank;
    }

    // Step 3: strip outer whitespace.
    let joined = collapsed.join("\n");
    joined.trim().to_string()
}

/// SHA-256 of `normalized`, hex-encoded (lowercase). 64 hex chars.
pub fn content_hash(normalized: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for byte in digest {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(b.len() / 4);
    let mut buf = [0u8; 4];
    let mut i = 0;
    while i + 4 <= b.len() {
        buf.copy_from_slice(&b[i..i + 4]);
        out.push(f32::from_le_bytes(buf));
        i += 4;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use engram_llm::{
        CompleteOptions, Completion, EmbeddingModel, LlmProvider, Model, ModelProvider,
        PromptStructured, StreamedCompletion,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts how many times `embed()` was called and what model name was
    /// requested. Lets tests assert cache behavior end-to-end.
    struct CountingProvider {
        calls: AtomicUsize,
        dim: usize,
    }

    impl CountingProvider {
        fn new(dim: usize) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                dim,
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for CountingProvider {
        async fn complete(
            &self,
            _: &PromptStructured,
            _: &Model,
            _: &CompleteOptions,
        ) -> engram_llm::Result<Completion> {
            unimplemented!("CountingProvider does not implement complete")
        }
        async fn complete_streamed(
            &self,
            _: &PromptStructured,
            _: &Model,
            _: &CompleteOptions,
        ) -> engram_llm::Result<StreamedCompletion> {
            unimplemented!("CountingProvider does not implement complete_streamed")
        }
        async fn embed(&self, text: &str, _model: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Deterministic: hash text into the first byte, broadcast.
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            let digest = hasher.finalize();
            Ok((0..self.dim)
                .map(|i| ((digest[i % 32]) as f32) / 255.0)
                .collect())
        }
    }

    fn fresh_pipeline(dim: usize) -> (EmbeddingPipeline, Arc<CountingProvider>) {
        // In-memory SQLite + minimal table — we only need the
        // `embedding_cache` table, not the full migration.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE embedding_cache (
                content_hash  TEXT NOT NULL,
                model         TEXT NOT NULL,
                model_version TEXT NOT NULL,
                dimensions    INTEGER NOT NULL,
                embedding     BLOB NOT NULL,
                first_seen_at TEXT NOT NULL,
                last_used_at  TEXT NOT NULL,
                use_count     INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (content_hash, model, model_version, dimensions)
            );",
        )
        .unwrap();
        let provider = Arc::new(CountingProvider::new(dim));
        let pipeline = EmbeddingPipeline::new(conn, provider.clone());
        (pipeline, provider)
    }

    fn model(name: &str, dim: usize) -> EmbeddingModel {
        EmbeddingModel {
            provider: ModelProvider::OpenAi,
            name: name.to_string(),
            dim,
        }
    }

    // ── normalize_for_hash ──────────────────────────────────────────────

    #[test]
    fn normalize_trims_trailing_whitespace_per_line() {
        assert_eq!(
            normalize_for_hash("line one   \nline two \t \n"),
            "line one\nline two"
        );
    }

    #[test]
    fn normalize_collapses_blank_line_runs() {
        assert_eq!(normalize_for_hash("a\n\n\n\nb"), "a\n\nb");
    }

    #[test]
    fn normalize_strips_outer_whitespace() {
        assert_eq!(normalize_for_hash("\n\n   hello\n\n"), "hello");
    }

    #[test]
    fn normalize_is_stable_across_cosmetic_edits() {
        let raw_a = "line 1\nline 2";
        let raw_b = "line 1   \nline 2  ";
        let raw_c = "line 1\n\n\n\nline 2";
        let raw_d = "  line 1\nline 2  \n\n";
        assert_eq!(
            content_hash(&normalize_for_hash(raw_a)),
            content_hash(&normalize_for_hash(raw_b))
        );
        // raw_c has internal blank lines collapsed, but raw_a has none —
        // these intentionally hash differently. Verify cosmetic edits to
        // the trailing whitespace and leading/trailing blank lines do not
        // change the hash, but adding paragraph breaks does.
        assert_eq!(
            content_hash(&normalize_for_hash(raw_a)),
            content_hash(&normalize_for_hash(raw_d))
        );
        assert_ne!(
            content_hash(&normalize_for_hash(raw_a)),
            content_hash(&normalize_for_hash(raw_c))
        );
    }

    #[test]
    fn content_hash_is_64_hex_chars() {
        let h = content_hash("anything");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── pipeline behavior ────────────────────────────────────────────────

    #[tokio::test]
    async fn first_call_misses_and_calls_provider() {
        let (pipeline, provider) = fresh_pipeline(4);
        let m = model("test-emb", 4);
        let out = pipeline.embed("hello world", &m).await.unwrap();
        assert!(!out.cache_hit);
        assert_eq!(out.dimensions, 4);
        assert_eq!(out.vector.len(), 4);
        assert_eq!(provider.calls(), 1);
    }

    #[tokio::test]
    async fn second_call_hits_cache_no_provider() {
        let (pipeline, provider) = fresh_pipeline(8);
        let m = model("test-emb", 8);
        let _ = pipeline.embed("repeated text", &m).await.unwrap();
        let out2 = pipeline.embed("repeated text", &m).await.unwrap();
        assert!(out2.cache_hit, "second call must be a hit");
        assert_eq!(provider.calls(), 1, "provider must not be called again");
    }

    #[tokio::test]
    async fn cache_returns_identical_vector_across_calls() {
        let (pipeline, _) = fresh_pipeline(16);
        let m = model("test-emb", 16);
        let a = pipeline.embed("same text", &m).await.unwrap();
        let b = pipeline.embed("same text", &m).await.unwrap();
        assert_eq!(a.vector, b.vector);
    }

    #[tokio::test]
    async fn cosmetic_edits_share_cache_entry() {
        let (pipeline, provider) = fresh_pipeline(4);
        let m = model("test-emb", 4);
        let _ = pipeline.embed("line a\nline b", &m).await.unwrap();
        // Trailing whitespace edit — same normalized form, same hash.
        let _ = pipeline.embed("line a   \nline b  ", &m).await.unwrap();
        assert_eq!(
            provider.calls(),
            1,
            "cosmetic edit must hit the existing cache row"
        );
    }

    #[tokio::test]
    async fn different_text_misses_separately() {
        let (pipeline, provider) = fresh_pipeline(4);
        let m = model("test-emb", 4);
        let _ = pipeline.embed("alpha", &m).await.unwrap();
        let _ = pipeline.embed("beta", &m).await.unwrap();
        assert_eq!(provider.calls(), 2);
    }

    #[tokio::test]
    async fn different_models_miss_separately() {
        let (pipeline, provider) = fresh_pipeline(4);
        let m1 = model("model-a", 4);
        let m2 = model("model-b", 4);
        let _ = pipeline.embed("shared text", &m1).await.unwrap();
        let _ = pipeline.embed("shared text", &m2).await.unwrap();
        assert_eq!(
            provider.calls(),
            2,
            "different model names must produce different cache rows"
        );
    }

    #[tokio::test]
    async fn bypass_cache_always_calls_provider() {
        let (pipeline, provider) = fresh_pipeline(4);
        let m = model("test-emb", 4);
        let _ = pipeline.embed("cached", &m).await.unwrap();
        assert_eq!(provider.calls(), 1);
        let bypass = pipeline.embed_bypassing_cache("cached", &m).await.unwrap();
        assert!(!bypass.cache_hit);
        assert_eq!(
            provider.calls(),
            2,
            "bypass must hit the provider regardless of cache state"
        );
    }

    #[tokio::test]
    async fn dimension_mismatch_surfaces_error() {
        struct WrongDimProvider;
        #[async_trait]
        impl LlmProvider for WrongDimProvider {
            async fn complete(
                &self,
                _: &PromptStructured,
                _: &Model,
                _: &CompleteOptions,
            ) -> engram_llm::Result<Completion> {
                unimplemented!()
            }
            async fn complete_streamed(
                &self,
                _: &PromptStructured,
                _: &Model,
                _: &CompleteOptions,
            ) -> engram_llm::Result<StreamedCompletion> {
                unimplemented!()
            }
            async fn embed(&self, _: &str, _: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
                // Lies — returns a 3-dim vector for a 4-dim model.
                Ok(vec![0.1, 0.2, 0.3])
            }
        }
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE embedding_cache (
                content_hash TEXT NOT NULL, model TEXT NOT NULL,
                model_version TEXT NOT NULL, dimensions INTEGER NOT NULL,
                embedding BLOB NOT NULL, first_seen_at TEXT NOT NULL,
                last_used_at TEXT NOT NULL, use_count INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (content_hash, model, model_version, dimensions));",
        )
        .unwrap();
        let provider: Arc<dyn LlmProvider> = Arc::new(WrongDimProvider);
        let pipeline = EmbeddingPipeline::new(conn, provider);
        let m = model("test-emb", 4);
        let err = pipeline.embed("anything", &m).await.unwrap_err();
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
    async fn hit_ratio_around_50_percent_for_50_percent_repeats() {
        // 1000 calls, 500 unique texts, each used twice.
        let (pipeline, provider) = fresh_pipeline(8);
        let m = model("test-emb", 8);
        let mut total = 0;
        let mut hits = 0;
        for i in 0..500 {
            let text = format!("text-{i}");
            let r1 = pipeline.embed(&text, &m).await.unwrap();
            let r2 = pipeline.embed(&text, &m).await.unwrap();
            total += 2;
            if r1.cache_hit {
                hits += 1;
            }
            if r2.cache_hit {
                hits += 1;
            }
        }
        let ratio = hits as f64 / total as f64;
        assert!(
            (ratio - 0.5).abs() < 0.01,
            "expected ~50% hit ratio, got {ratio}"
        );
        assert_eq!(
            provider.calls(),
            500,
            "exactly one provider call per unique text"
        );
    }

    // ── blob encoding round-trip ─────────────────────────────────────────

    #[test]
    fn vec_blob_round_trip() {
        let v = vec![0.0_f32, 1.0, -1.0, 0.5, -0.5, f32::MIN_POSITIVE, f32::MAX];
        let blob = vec_to_blob(&v);
        let back = blob_to_vec(&blob);
        assert_eq!(v, back);
    }

    #[test]
    fn vec_blob_packs_4_bytes_per_float() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let blob = vec_to_blob(&v);
        assert_eq!(blob.len(), 12);
    }

    // ── property tests ───────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// Hash is stable under trailing-whitespace cosmetic edits.
        #[test]
        fn prop_hash_stable_under_trailing_whitespace(
            text in "[a-z0-9 \n]{0,500}",
            suffix in " *"
        ) {
            // Build a "noisy" version that adds `suffix` to the end of each
            // line. The normalized hash must be identical.
            let noisy: String = text
                .split('\n')
                .map(|l| format!("{l}{suffix}"))
                .collect::<Vec<_>>()
                .join("\n");
            let h1 = content_hash(&normalize_for_hash(&text));
            let h2 = content_hash(&normalize_for_hash(&noisy));
            prop_assert_eq!(h1, h2);
        }

        /// Hash output is always 64 lowercase hex characters.
        #[test]
        fn prop_hash_shape(text in any::<String>()) {
            let h = content_hash(&normalize_for_hash(&text));
            prop_assert_eq!(h.len(), 64);
            prop_assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
