# ADR 0014: LanceDB for vector storage in v1 (supersedes sqlite-vec)

**Status:** Accepted

**Date:** 2026-05 (revised vector-storage choice during the "v1 feature-complete" pass; pulls LanceDB forward from its previous v3+ scale-out position)

**Supersedes:** the original sqlite-vec recommendation in earlier revisions of `03-architecture.md`.

## Context

The earlier design used **sqlite-vec** (a SQLite extension) for vector similarity search. sqlite-vec is the natural fit for "everything in one SQLite file" — simple, low-operational-overhead, fast enough at v1 scale (10K notes × 1024 dims).

But two pressures argue for changing this choice for v1:

1. **Engram is intended to last.** A user who builds with engram for years should not hit a hard re-architecture wall the moment their vault crosses 50K-100K notes. sqlite-vec uses brute-force scan by default (or HNSW with v0.2+, but the implementation is younger than alternatives) and slows linearly with collection size. At 100K vectors p95 query latency hits ~200ms; at 1M it falls over.
2. **The v1-feature-complete intent.** The user has stated that v1 should be feature-complete for the system's intended shape, which includes scale headroom. Re-architecting the vector store later means re-embedding the entire vault, migrating retrieval-pipeline code, and re-running the eval framework against the new store. Doing this upfront is cheaper.

**[LanceDB](https://lancedb.com/)** is a Rust-native embedded vector database with:

- Columnar storage (Lance format, an Arrow-compatible format optimized for ML workloads)
- Native HNSW and IVF vector indices, plus brute-force for small collections
- Embedded (no separate server process); same single-binary deployment story
- Built-in versioning + time-travel queries (useful for the audit/provenance story)
- Mature Rust SDK (`lance` + `lancedb` crates)
- Used in production at scale by multiple teams

The cost of adopting LanceDB in v1: a small operational addition (a `.engram/vectors/` directory alongside `.engram/index.sqlite`); slightly more complex backup; eventually-consistent vector writes vs. the strict SQLite-transaction model.

The benefit: a vector store that genuinely scales with the user's vault, with cleaner-from-day-one semantics for the LLM-era retrieval workload.

## Decision

**Use LanceDB as the canonical vector store for v1.** sqlite-vec is removed from the architecture.

### Storage layout

```
.engram/
  index.sqlite              # everything except dense embeddings (per prior design)
  vectors/                  # LanceDB dataset directory (new)
    notes_v1.lance/         # one dataset per embedding-model-version
    _versions/              # LanceDB's built-in versioning
    _transactions/
```

The `vectors/` directory holds one LanceDB dataset per active `(model, version, dimensions)` combination. Switching models doesn't blow away the old dataset (per [ADR 0012](0012-embedding-cache-by-content-hash.md) — the embedding cache is keyed by model identity; LanceDB datasets are too).

### Schema (LanceDB dataset)

```python
# Conceptual schema; actual definition is in engram-index via the Rust lance crate
notes_v1 (dataset)
├── id: utf8 (note ULID)
├── content_hash: utf8 (matches embedding_cache.content_hash for reconciliation)
├── embedding: fixed_size_list<float32, 1024>  # dim per active model
├── note_type: utf8
├── created_at: timestamp_ms_utc
├── modified_at: timestamp_ms_utc
└── tags: list<utf8>
```

A small set of duplicate columns (`note_type`, `created_at`, `tags`) live alongside the vector to enable filtered vector search without an extra SQLite roundtrip ("find similar notes that are evergreen AND created in the last 90 days").

The `id` column is the join key back to SQLite for everything else.

### Vector index

- **HNSW** for ≥1000 vectors (the practical floor where HNSW outperforms brute-force).
- **Brute-force** below that threshold (new vaults, individual sub-collections).
- Index parameters tuned per `(model, dim)`. Defaults: `ef_construction = 100`, `m = 16` (LanceDB defaults, well-validated).

### Write semantics

LanceDB writes are **eventually consistent with SQLite**. The canonical "does this note exist?" answer lives in SQLite's `notes` table. The vector store is "is this note findable via semantic search?" — allowed to be slightly behind the canonical state.

**Atomic-write flow (updated from `03-architecture.md`):**

```
1. Begin write_intents row (SQLite transaction)
2. Write markdown to <path>.tmp
3. Write sidecar to <sidecar>.tmp
4. Update SQLite: notes + agent_actions + embedding_cache rows
5. fsync .tmp files
6. Atomic renames (markdown, sidecar)
7. Commit SQLite transaction → state visible to readers
8. Async: upsert into LanceDB collection (best-effort; retried by reconciliation if it fails)
9. Mark write_intent committed
```

If step 8 fails or crashes mid-write, the next file-change event or scheduled reconciliation pass detects: "SQLite says this note has content_hash X; LanceDB has no row for this id OR has a row with different content_hash" → re-upsert. The reconciliation logic lives in the indexer's startup sequence and runs periodically (hourly).

**This means a freshly-modified note may not be findable via semantic search for up to a few seconds.** The retrieval pipeline's BM25 + graph layers fill the gap during this window.

### Backup

LanceDB datasets are directory-based (multi-file). Backup model:

- **Vault + sidecars + `.engram/index.sqlite`** — always back up (per existing design).
- **`.engram/vectors/`** — conditionally back up. The vectors are _technically_ rebuildable from `embedding_cache` + the vault (re-embed using the cached vectors, repopulate LanceDB). But rebuild time at 10K notes is ~10 min; at 100K it's substantial. Backing up the dataset directly skips this cost.

Recommended: include `.engram/vectors/` in routine backup. The full restore path is documented in `03-architecture.md` §Backup and disaster recovery, updated to include LanceDB.

### LanceDB versioning + audit value

LanceDB's built-in dataset versioning is genuinely useful for engram:

- Every write creates a new version; old versions are queryable until garbage-collected.
- Audit-style queries become possible: "what was the embedding for this note 6 months ago?" — useful for Auditor's quarterly review when an agent's behavior drifts.
- Roll-back semantics: if a bulk re-embedding goes wrong, roll the dataset back to the previous version.

Garbage collection of old versions is scheduled monthly (configurable). Recent versions (last 7 days) always kept.

### Embedding cache interaction

[ADR 0012](0012-embedding-cache-by-content-hash.md) (embedding cache by content hash) still applies and remains the source of truth for computed embeddings. The flow:

1. Need embedding for content C with model M → check `embedding_cache` keyed by `(sha256(C), M, version, dim)`.
2. If hit: use the cached vector. Upsert into LanceDB if the dataset doesn't already have this `(id, content_hash)` pair.
3. If miss: compute embedding via the LLM provider; store in both `embedding_cache` AND LanceDB.

The cache catches "we already computed this vector" (across notes with identical content, or after a `git restore` brings a previous content state back). LanceDB stores the vectors in the format optimized for ANN queries.

## Alternatives considered

1. **Keep sqlite-vec for v1; migrate to LanceDB at v3+.** The original plan. Rejected because: (a) migrating the vector store later means re-embedding the entire vault + reworking retrieval-pipeline code + re-running evals — substantial; (b) v1 is the right time to make this choice if engram is intended to last.
2. **Qdrant** (standalone server). Rejected: violates the single-binary deployment story; adds a separate process to manage, secure, and back up.
3. **Chroma** (embedded). Considered. LanceDB has better Rust support, better backing organization, and a more mature dataset-versioning story.
4. **Milvus / Weaviate / Pinecone** — distributed / cloud vector DBs. Wildly over-engineered for a single-user local tool.
5. **Hand-rolled** HNSW in Rust (e.g., `instant-distance` or `hnsw_rs`). Rejected: reinventing the wheel for a problem LanceDB solves cleanly.
6. **LanceDB embedded in v1.** Chosen.

## Decision rationale

- **Scale headroom from day one.** Engram supports 100K+ notes without re-architecture. Aligns with "intended to last."
- **Same single-binary deployment story.** No new process; LanceDB compiles into the `engram` binary alongside SQLite.
- **Rust-native.** First-class Rust SDK; integrates cleanly with `engram-index` crate.
- **Columnar advantages.** Filtered vector search (semantic + metadata predicates) is native to the Lance format. No need to round-trip to SQLite to filter results.
- **Versioning is genuinely useful.** Audit / time-travel queries align with engram's provenance story.
- **Eventual consistency is acceptable** because the retrieval pipeline has BM25 + graph layers; a few hundred ms of vector-store lag never produces incorrect results, just slightly stale semantic search for freshly-modified notes.
- **Composes with the embedding cache (ADR 0012).** Cache + LanceDB are complementary: cache prevents re-embedding; LanceDB makes the existing vectors searchable.

## Consequences

**Positive:**

- Scale headroom without re-architecture.
- Better filtered vector search (semantic + tag/type/date predicates in one pass).
- Built-in versioning + time-travel.
- Cleaner separation of concerns: SQLite owns "what exists"; LanceDB owns "what's similar to what."

**Negative:**

- **Two storage formats.** Backup, migration, and reconciliation logic must handle both. Mitigated by the eventual-consistency model and the embedding cache as a recovery source.
- **Eventually-consistent vector writes.** A note modified at t=0 may not appear in semantic search until t=0.5s. Mitigated by BM25/graph in the retrieval pipeline; documented as a known property.
- **More complex atomic-write flow.** The triple-write story (markdown + sidecar + SQLite) was already complex; LanceDB adds a fourth target with weaker consistency. Mitigated by treating LanceDB as a downstream cache that reconciles from SQLite.
- **LanceDB ecosystem younger than SQLite.** Mature for the problem, but less battle-tested than SQLite for edge cases. Mitigated by pinning to stable releases and integration tests covering the engram-specific patterns.
- **Slightly larger binary.** LanceDB pulls Arrow dependencies. Acceptable.

## Migration path

For users who somehow end up on a sqlite-vec engram build (none should, since v1 ships with LanceDB), a one-shot migration command exists:

```
engram vectors migrate --from sqlite-vec --to lancedb
```

Reads from the old `notes_vec` virtual table; writes a fresh LanceDB dataset. Documented in `03-architecture.md` §Schema migrations.

## References

- [`03-architecture.md`](../03-architecture.md) — tech stack, crate workspace, retrieval pipeline (all updated for LanceDB)
- [`10-performance-budgets.md`](../10-performance-budgets.md) — updated vector-search latency targets
- [ADR 0012](0012-embedding-cache-by-content-hash.md) — embedding cache, which feeds LanceDB
- [LanceDB documentation](https://lancedb.com/docs)
- [Lance file format spec](https://lancedb.github.io/lance/)
