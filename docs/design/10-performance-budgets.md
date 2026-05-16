# Performance Budgets

## Purpose

"Excellent" is meaningless without numbers. This doc defines quantitative targets for engram's primary operations: indexer throughput, query latency, agent turnaround, capture latency, ingestion duration, memory footprint, disk usage, and cost. These are the targets against which "engineering excellence" is measured.

Targets are sized for **a single user with a 10K-note vault on Apple Silicon (M-series)**. At 50K+ notes, the architecture may need re-thinking; that's flagged in `00-overview.md` as an open question.

## Definitions

- **p50 / p95 / p99** — latency percentiles measured over a representative sample window (typically 1 day of operation).
- **Steady state** — after vault is fully indexed and the system has settled (file watcher idle, no in-flight agent runs).
- **Cold start** — first invocation after `engram serve` starts.
- **Hot path** — any operation visible to the user (UI, capture, search). Cold paths (background indexing, agent runs, dream mode) have looser budgets.

## Hot path budgets (user-facing)

These are the operations the user notices. Misses here register as "the app feels slow."

| Operation | p50 | p95 | p99 | Notes |
|---|---|---|---|---|
| `/changes` (list pending unstaged changes) | 20ms | 50ms | 100ms | Just sqlite + git status; no LLM, no embedding |
| `/changes/:path/stage` | 30ms | 80ms | 150ms | One git add + one sqlite update |
| `/changes/:path/discard` | 30ms | 80ms | 150ms | One git restore + one sqlite update |
| `/notes/:id` | 10ms | 30ms | 80ms | sqlite + filesystem read |
| `/search?q=...` | 60ms | 200ms | 400ms | Hybrid: BM25 + dense + RRF + rerank + graph |
| `/graph/:id` (1-hop neighborhood) | 20ms | 60ms | 120ms | Pure sqlite |
| `/biography` | 5ms | 20ms | 50ms | Single file read |
| `/standup` (today's report) | 100ms | 300ms | 500ms | Aggregates several queries; cached for 5min |
| Swift app capture submit (POST `/ingest`) | 200ms | 500ms | 1s | Just the queue ack; extraction is async |
| Swift app capture-to-acked | 300ms | 800ms | 2s | When server is reachable; offline queue handles unreachable |
| Swift app cold launch to first interactive | 400ms | 1s | 2s | SwiftUI native render |

**Rationale for `/search` p95 of 200ms:** below this threshold, search feels instant; above 400ms, the user notices. The retrieval pipeline (BM25 + dense + RRF + rerank + graph expansion) is bounded by the slowest stage; rerank is typically the cap (~50ms cross-encoder pass on 50 candidates).

## Capture path budgets

Capture is the highest-priority surface; the failure mode of a knowledge tool is friction at the entry point. Capture latency is measured separately because it spans Swift app + network + server.

| Operation | p50 | p95 | p99 | Notes |
|---|---|---|---|---|
| Voice memo dictation start (Swift app) | 100ms | 300ms | 600ms | iOS speech framework startup |
| Capture submit to local queue ack | 50ms | 100ms | 200ms | Pure local SwiftData write |
| Local queue → server ack (online) | 200ms | 500ms | 1s | Network + sqlite write |
| Local queue → server ack (offline → reconnect) | n/a | 30s | 5min | Bounded by network availability |
| Share-extension launch | 200ms | 500ms | 1s | iOS share extension startup is the dominant cost |
| Drag-drop (macOS) submit | 50ms | 150ms | 300ms | Local IPC |

## Ingestion budgets

Ingestion is async; the user's perception is dominated by the queue ack (above). Extraction time matters for "when does the literature note land in review queue."

| File type | p50 | p95 | p99 | Notes |
|---|---|---|---|---|
| Plain text / markdown | 200ms | 500ms | 1s | Just Scribe + Linker proposal |
| PDF (cloud vision, < 20 pages) | 5s | 10s | 20s | Claude vision dominant cost |
| PDF (local OCR fallback, < 20 pages) | 10s | 30s | 60s | `ocrs` is slower than vision |
| Image / screenshot (cloud vision) | 2s | 5s | 10s | One vision call |
| Web URL (readability extraction) | 1s | 3s | 8s | Network + parse |
| Voice memo (< 1 min, local Whisper) | 3s | 8s | 15s | `whisper.cpp` on Apple Silicon |
| Long audio (1 hour, chunked local Whisper) | 5min | 10min | 20min | Chunked + parallelized |
| Audio (cloud Whisper API) | 10s | 30s | 60s | Network + API |

## Background path budgets

These are not user-facing but bound system load.

| Operation | Throughput | Notes |
|---|---|---|
| Indexer | 100 notes/sec on Apple Silicon | sqlite + FTS5 + sidecar parse |
| Embedding (local bge-m3 via ONNX) | 50 notes/sec | Apple Silicon GPU |
| Embedding (cloud OpenAI batch) | 200 notes/sec | Network-bound |
| Vector search (LanceDB HNSW) | < 30ms p95 for 10K notes; < 80ms p95 for 100K | Single ANN query with optional metadata filter |
| BM25 (FTS5) | < 10ms p95 for 10K notes | Sqlite native |
| Graph expansion (1--2 hop) | < 30ms p95 | sqlite recursive CTE |
| File watcher debounce window | 2s default | Configurable; trade-off: too short = thrash, too long = lag |
| Reindex full (10K notes) | < 10 min | Includes embedding |
| Sidecar read | < 1ms | Single file |
| Sidecar write | < 5ms | Includes pretty JSON serialize |

## Agent turnaround budgets

Per-agent timing is bounded by LLM provider latency. Targets assume cloud Anthropic for `standard`/`deep` tiers and local for `fast`.

| Operation | p50 | p95 | p99 | Notes |
|---|---|---|---|---|
| Agent run (single mechanical, e.g., Linker) | 1s | 3s | 8s | One LLM call (`fast` tier) |
| Agent run (single thinking, e.g., Synthesizer) | 5s | 15s | 30s | One LLM call (`standard` tier) |
| Agent run (deep, e.g., Heretic, Analogist) | 15s | 45s | 90s | One LLM call (`deep` tier) |
| Council deliberation (3--5 agents, no revision) | 8s | 25s | 60s | Parallelized critique calls |
| Council deliberation (with revision round) | 15s | 50s | 120s | Two rounds, sequential |
| Research Council briefing | 30s | 90s | 180s | Multi-agent, deep tier |
| Untangler map | 20s | 60s | 120s | Multi-agent, standard tier |
| Pair-Thinking turn | 3s | 8s | 15s | One question/response cycle |

## Memory and disk budgets

| Component | Target |
|---|---|
| `engram serve` resident memory (steady state, 10K notes) | < 500MB |
| `engram serve` resident memory (peak during reindex) | < 1.5GB |
| Swift app memory (steady state) | < 200MB |
| Swift app memory (during voice transcription) | < 500MB |
| `.engram/index.sqlite` (10K notes; metadata + FTS5 + graph + cache, no vectors) | < 60MB |
| `.engram/vectors/` (10K notes × 1024 dims, LanceDB compressed) | < 50MB |
| `.engram/sidecar/` total (10K notes, typical) | < 50MB |
| `.engram/artifacts/` | unbounded (user content); monitored, not capped |
| `.engram/logs/` | < 100MB rotated |

## Cost budgets

The user must never be surprised by their bill. Cost targets assume cloud Anthropic + cloud OpenAI embeddings. Local-only operation is much cheaper but requires more compute.

| Profile | Monthly target | Notes |
|---|---|---|
| Light user (capture-only, ~100 notes/month) | < $5 | Mostly embedding cost |
| Active user (v1 agent set, ~500 notes/month, daily standup) | < $30 | Roughly 80% LLM, 20% embedding |
| Power user (full agent set, council-heavy) | < $100 | After v2.2 ships |
| Cap default | $50/month | Configurable in `.engram/config.toml` |

System-wide cost cap is enforced by Watcher (see `03-architecture.md`).

## Scale ceiling

Targets above hold for 10K notes. Notable limits:

- **LanceDB (HNSW)** scales gracefully to ~1M vectors on modest hardware; at v1's 10K-note target it's overprovisioned. Per [ADR 0014](adrs/0014-lancedb-vector-storage.md).
- **FTS5** scales well; not the bottleneck.
- **Link graph** scales well via sqlite indexes; not the bottleneck.
- **Embedding pipeline** is the dominant cost at scale; cloud embedding cost grows linearly with note count.
- **Agent run frequency** scales with vault activity, not vault size; modestly affected by larger vaults (more retrieval work per run).

At 50K+ notes, expect to revisit:
- Embedding model dimensionality (Matryoshka reduction to 256 or 512 dims)
- Vector index structure (HNSW, IVF)
- Sidecar storage strategy (consider sqlite-blob for very small sidecars, files for larger)

## Measurement methodology

- **Instrumentation:** every meaningful function in `engram-core` is annotated with `tracing` spans. Span attributes include operation name, duration, byte count, error.
- **Metrics emission:** spans aggregate into RED metrics (Rate, Errors, Duration) per endpoint and per agent. Optionally exported via OpenTelemetry.
- **Local visibility:** `engram status --metrics` prints current p50/p95/p99 for the past hour per operation.
- **Regression detection:** Auditor's quarterly run includes a performance section. If any operation's p95 has degraded > 25% quarter-over-quarter, surfaced as a finding.

## Anti-goals

These are NOT engram's performance commitments:

- **Sub-millisecond response times.** Engram is a personal tool, not a real-time system.
- **Concurrent multi-user load.** Engram is single-user; concurrency planning targets one user with multiple devices, not many users.
- **High-throughput ingestion.** Bursts of 1000 captures/minute are not optimized for. Bounded queue + backpressure handles bursts gracefully but slowly.
- **GPU memory optimization.** Local LLM and embedding models use whatever's reasonable; users with large vaults should use cloud providers.
- **Streaming everywhere.** Most agent responses are batch; streaming is reserved for SSE-event subscribers (Swift app live updates, conversation surfaces).

## Failure modes and degradation

When budgets are exceeded:

- **Search slow:** index may be stale; suggest `engram reindex`.
- **Capture slow (server reachable):** likely embedding pipeline saturation; queue items pile up; Pacekeeper throttles producing agents.
- **Capture slow (server unreachable):** local queue handles indefinitely; Swift app shows queue depth + offline indicator.
- **Agent runs slow:** likely cloud provider latency; switch to local `fast` model for affected agent.
- **Cost cap hit:** all LLM-using agents pause; user notified; embedding/indexing/local-model work continues.
- **Memory ceiling exceeded:** Watcher logs warning; user can run `engram restart` to reclaim.
