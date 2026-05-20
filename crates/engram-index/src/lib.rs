//! SQLite metadata index, FTS5 full-text search, LanceDB vector storage, and hybrid retrieval.

/// SQLite connection management, schema migrations, and table access.
pub mod sqlite;

/// SQLite FTS5 full-text search index (BM25 scoring).
pub mod fts {}

/// LanceDB embedded vector store at `.engram/vectors/`.
pub mod vectors {}

/// Wikilink and backlink graph stored in SQLite.
pub mod link_graph {}

/// Tag graph and hierarchical tag resolution.
pub mod tag_graph {}

/// Embedding pipeline: local ONNX (bge-m3) and cloud OpenAI fallback.
pub mod embeddings {}

/// Hybrid retrieval: BM25 + dense ANN + RRF fusion + cross-encoder rerank + graph expansion.
pub mod search {}
