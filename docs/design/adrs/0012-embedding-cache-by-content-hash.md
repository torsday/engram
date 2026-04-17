# ADR 0012: Embedding cache keyed by content hash and model version

**Status:** Accepted

**Date:** 2026-05 (added during the agentic-AI + token-efficiency excellence pass)

## Context

Engram embeds every note (and many smaller text spans) for hybrid retrieval. Embedding has three cost dimensions:

- **Cloud:** real dollars per call (OpenAI `text-embedding-3-large`: ~$0.13 per 1M tokens; for a 10K-note vault at avg 500 tokens, ~$0.65 to fully re-embed).
- **Local:** real GPU/CPU time (`bge-m3` via ONNX on Apple Silicon: ~50 notes/sec; full reindex of 10K notes ≈ 200s).
- **Indirect:** any operation that triggers re-embedding (model switch, schema migration, manual reindex) compounds the cost.

The naïve approach — re-embed every note on every change event — is wasteful. A note's filename changes; its content doesn't; we shouldn't re-embed. A note's content gets one whitespace fix; we re-embed because the bytes differ.

The architectural decision: **cache embeddings by `(content_hash, embedding_model, embedding_dim)` and serve from cache when the key matches.**

## Decision

Maintain an embedding cache indexed by content hash and model identity. The cache is consulted before any embedding call; cache misses trigger a real embedding and write back the result.

### Cache schema

A new SQLite table inside the existing `index.sqlite`:

```sql
CREATE TABLE embedding_cache (
    content_hash    TEXT NOT NULL,         -- SHA-256 of the embedded text (UTF-8 bytes)
    model           TEXT NOT NULL,         -- e.g. "bge-m3" | "text-embedding-3-large"
    model_version   TEXT NOT NULL,         -- e.g. "1.5" | "2024-01-25"
    dimensions      INTEGER NOT NULL,
    embedding       BLOB NOT NULL,         -- packed float32 vector
    first_seen_at   TEXT NOT NULL,         -- ISO 8601 UTC
    last_used_at    TEXT NOT NULL,
    use_count       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (content_hash, model, model_version, dimensions)
);
CREATE INDEX idx_embedding_cache_lru ON embedding_cache(last_used_at);
```

### Hash semantics

The hash is over the **embeddable content**, not the entire markdown file:

- For a whole-note embedding: SHA-256 of `(title + "\n\n" + body)` after frontmatter strip and standard whitespace normalization (trim trailing whitespace per line; collapse runs of blank lines to one).
- For a section-level embedding (used by Splitter, Inquirer's blindspot mode): SHA-256 of just the section text.
- Normalization is deterministic and documented. Identical *normalized* content → identical hash.

### Lookup flow

```
embed(text, model, version) -> Vector
  hash = sha256(normalize(text))
  if let Some(cached) = embedding_cache.get((hash, model, version)):
    cached.last_used_at = now
    cached.use_count += 1
    return cached.embedding
  vector = provider.embed(text, model, version)   // real call
  embedding_cache.insert(hash, model, version, vector, now)
  return vector
```

### Multi-model coexistence

Different models produce different vectors. The cache key includes model identity, so multiple model variants coexist:

```
content_hash=abc123 model=bge-m3       version=1.5  dim=1024  -> vector A
content_hash=abc123 model=text-embed-3 version=2024-01 dim=1536 -> vector B
```

This means **switching models doesn't blow away the old embeddings**. The vector store (`notes_vec`) uses the user's currently-active model; switching is a re-embedding migration, but the cached old vectors are kept until garbage collection (so reverting to a previous model is fast).

### Garbage collection

The Gardener agent's quarterly pass includes an embedding-cache GC step:

- Drop entries with `last_used_at` older than 180 days AND not matching any current vault note's hash.
- Drop entries for models that haven't been used in any embedding call for 180 days.
- Total cap: 100K entries; LRU eviction beyond.

GC is conservative (180-day window) because re-embedding is recoverable but cached entries are not.

### Hit-rate metrics

Per-month aggregate in `agent_runs` and exposed via `engram status`:

- `embeddings_requested`
- `embeddings_cache_hit`
- `cache_hit_ratio = hit / requested`
- `tokens_saved_via_cache` (estimated: hit_count × avg_tokens_per_call × cost_per_token)

Surfaced in cost dashboard: "Cache saved you ~$3.40 / 38,000 tokens this month."

### Bypass mode

For correctness testing or model evaluation, the cache can be bypassed via `--no-embedding-cache` CLI flag or `bypass_cache: true` per-call config. Useful when comparing model-version A vs. B on identical inputs.

## Alternatives considered

1. **No cache, re-embed on every event.** Simplest. Rejected: wasteful; full reindex compounds cost; modified-but-content-unchanged events (rename, frontmatter tweak) trigger pointless re-embeds.
2. **Cache by note ID + modified-at timestamp.** Works for ID-tracked notes but misses two important cases: (a) two notes with identical content (rare but real for boilerplate or quoted text) get embedded twice; (b) reverting a note to a previous content state re-embeds the same content. Hash-keyed cache handles both for free.
3. **In-memory only LRU.** Faster lookup, no persistence. Rejected: a process restart blows the cache; a quarterly Auditor sample run shouldn't trigger a full re-embed.
4. **Cache in SQLite with content-hash + model-version key.** Chosen.
5. **External KV store (sled, redis).** Overkill for a single-user system; sqlite is already in the dependency graph.

## Decision rationale

- **Real cost reduction.** For a vault that grows ~50 notes/month with ~10% modification rate on existing notes, the cache saves ~95% of re-embed calls vs. the naïve approach.
- **Migration safety.** Switching embedding models doesn't lose the old vectors; reverting is fast.
- **Simple correctness model.** Hash-keyed cache has clear invariants: same hash + same model + same version → same vector. No subtle staleness bugs.
- **Composes with other optimizations.** Cache misses still benefit from prompt caching (ADR 0010) and tier escalation (ADR 0011) at the embedding-call level (cloud OpenAI batches; local bge-m3 fast).
- **Operationally observable.** Hit rate is a single metric; sub-90% on a stable vault indicates a problem worth investigating.

## Consequences

**Positive:**

- **~95% cost reduction** for embedding workload on a stable vault.
- **Safe model switching** — old vectors preserved, can revert without recomputation.
- **Reindexing is cheap** when content hasn't changed (e.g., schema migration that doesn't touch markdown).
- **Cache hit ratio is a system-health metric** — surfaces issues like content-normalization bugs (low hit rate suggests hashes aren't stable across writes).

**Negative:**

- **SQLite size grows.** 10K notes × 1024 dims × 4 bytes ≈ 40MB just for the cache. Bounded; not a problem at v1 scale; flagged for re-architecture above 100K entries.
- **Normalization correctness matters.** A subtle bug in `normalize()` could hash identical content differently, defeating the cache. Mitigation: property-based test (`normalize(s) == normalize(normalize(s))` and `hash(normalize(a)) == hash(normalize(b))` for `a` and `b` differing only in whitespace).
- **GC must be conservative.** Aggressive GC could evict entries that get reused. The 180-day window is intentional; can be tuned via Watcher feedback.
- **Cache invalidation on model upgrade is manual.** When `bge-m3` goes from v1.5 → v2.0, old entries (key includes version) are kept; new embeddings populate v2.0 entries. The active vector store needs a migration to switch over. Manual step documented in `engram migrate`.

## References

- [`03-architecture.md`](../03-architecture.md) — Index schema (the table above); embedding pipeline; performance budgets
- [`10-performance-budgets.md`](../10-performance-budgets.md) — embedding throughput targets that the cache directly affects
- [ADR 0010](0010-prompt-caching-first-class.md) — companion: prompt-level caching for LLM calls
- [ADR 0011](0011-tiered-model-escalation.md) — model-tier escalation; embedding model is independent (always at the configured embedding tier)
