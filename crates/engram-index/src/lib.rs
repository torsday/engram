//! SQLite metadata index, FTS5 full-text search, LanceDB vector storage, and hybrid retrieval.

/// SQLite connection management, schema migrations, and table access.
pub mod sqlite;

/// SQLite FTS5 full-text search index (BM25 scoring).
pub mod fts;

/// Atomic markdown + sidecar + SQLite writes via the `write_intents` log,
/// POSIX atomic-rename, and startup-time orphan recovery. See module docs
/// and `docs/design/03-architecture.md` §Atomic writes.
pub mod atomic_writes;

/// In-flight request deduplication for council retrieval calls.
pub mod coalesce;

/// LanceDB embedded vector store at `<vault_root>/vectors/`.
pub mod vector_store;

/// Wikilink and backlink graph stored in SQLite.
pub mod link_graph {}

/// Tag graph and hierarchical tag resolution.
pub mod tag_graph {}

/// Embedding pipeline: cache lookups, content-hash keying, provider dispatch.
pub mod embeddings;

/// SQLite ↔ LanceDB reconciliation: detects drift, stale vectors, and
/// orphans; re-embeds and corrects via the embedding pipeline.
pub mod reconcile;

/// Hybrid retrieval: BM25 + dense ANN + RRF fusion + cross-encoder rerank + graph expansion.
pub mod search {}
