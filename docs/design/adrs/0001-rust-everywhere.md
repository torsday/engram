# ADR 0001: Rust for the entire core, no Python or TypeScript agent layer

**Status:** Accepted

**Date:** 2026-04 (during initial design)

## Context

Engram is a long-running local service that watches files, manages indices, calls LLMs, orchestrates a multi-agent deliberation engine, hosts MCP servers, and ships next to a Swift app. The conventional choice for the LLM-orchestration layer of such a system would be Python (richest LLM ecosystem) or TypeScript (the official MCP SDK is TypeScript-native, fastest iteration on prompts and tools). The conventional choice for the file-watching, indexing, and SQLite layer would be Rust.

The conventional answer is therefore a hybrid: Rust core + Python or TypeScript agent layer, communicating via IPC or a shared database.

## Decision

Use **Rust for the entire core, including the agent host, council deliberation engine, and LLM provider abstraction.** No Python. No TypeScript agent layer. Prompts live in markdown files, hot-reloaded; the Rust code only sequences HTTP calls and enforces the deliberation protocol.

## Alternatives considered

1. **Hybrid Rust + Python.** Rust for indexer/watcher/git; Python for agent orchestration. Standard split.
2. **Hybrid Rust + TypeScript.** Same shape, TypeScript for agents (closer to MCP's reference implementation).
3. **Pure Python.** Simplest LLM ecosystem, but bad for long-running local services on user machines (packaging, deployment, performance).
4. **Pure Rust.** Chosen.

## Consequences

**Positive:**

- **Single static binary.** Ships next to the Swift app with no Python or Node runtime install. No version-pinning hell, no broken `pip` environments, no Electron weight.
- **Type-safe deliberation.** The state machine (`DRAFT -> CRITIQUE -> REVISE -> CONVERGE -> {LAND | PROPOSE | SHELVE}`), tool schemas, structured agent outputs (with `confidence`, `rationale`, `rubric_check` fields), and invasiveness ceilings all benefit from compile-time enforcement. In Python or TS these would be runtime checks at best.
- **Filesystem, SQLite, and vector-DB ergonomics.** `notify`, `gix`, `rusqlite`, `lance` / `lancedb` are all best-in-class Rust-native; the indexer, vector store, and git layer are written in their native idiom.
- **Performance under load.** A swarm of ~35 agents on a 10K-note vault with continuous file events is not a low-load scenario. Rust handles this without thinking; Python/Node would require careful work.
- **The LLM ecosystem gap is overstated.** We use cloud LLMs as remote services via HTTP. `async-openai`, `anthropic-rs`, and direct `reqwest` calls are perfectly serviceable. We do not train models or run heavy local inference; the LLM-ecosystem advantage of Python applies to use cases we don't have.

**Negative:**

- **Slower iteration on agent logic.** Compile times hurt when iterating on prompt-adjacent code. Mitigation: prompts are in markdown files, hot-reloaded at runtime --- the Rust code never recompiles to change agent behavior. Tool definitions and orchestration code do recompile, but that's the right granularity (tool changes are infrequent; prompt changes are frequent).
- **Smaller pool of contributors.** If engram ever takes contributions, "must know Rust" raises the bar. For now, single-developer; not relevant.
- **Rust MCP SDK is younger** than the TypeScript reference. Mitigation: the protocol is straightforward; we can implement what we need directly if `rmcp` is insufficient.

## References

- `03-architecture.md` --- tech stack table and crate workspace
- `01-agents-and-council.md` --- "agents are data, not code" principle (ADR 0002)
