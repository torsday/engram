# Architecture Decision Records

This directory holds short records of the load-bearing architectural decisions made for engram. Each ADR captures the context, the options considered, the decision made, and the consequences. The goal is to make the _why_ recoverable years later --- the design docs show the _what_; ADRs show _why it's that way_.

Format: each ADR is one page, in the form `NNNN-decision-name.md`. Status is `Accepted` for any decision currently in force; `Superseded` when revised; `Deprecated` when removed without replacement.

## Index

| #                                               | Decision                                                                       | Status   |
| ----------------------------------------------- | ------------------------------------------------------------------------------ | -------- |
| [0001](0001-rust-everywhere.md)                 | Rust for the entire core, no Python or TypeScript agent layer                  | Accepted |
| [0002](0002-agents-as-data.md)                  | Agents are directories of prompt + config, hot-reloaded                        | Accepted |
| [0003](0003-no-agent-commits.md)                | Agents never run `git add` or `git commit`                                     | Accepted |
| [0004](0004-confidence-gated-autonomy.md)       | Per-action confidence threshold gates autonomous writes                        | Accepted |
| [0005](0005-sidecar-json.md)                    | Rich agent metadata in sidecar JSON, not extended frontmatter                  | Accepted |
| [0006](0006-pure-title-slug-filenames.md)       | Filenames are pure title-slugs; ID lives in frontmatter only                   | Accepted |
| [0007](0007-steelman-rationality-gate.md)       | Steelman is a mandatory gate for all critical agents                           | Accepted |
| [0008](0008-two-mcp-servers.md)                 | Two MCP servers (internal stdio, external HTTP+SSE), not one                   | Accepted |
| [0009](0009-git-read-write-boundary.md)         | Git read/write boundary enforced at the type system                            | Accepted |
| [0010](0010-prompt-caching-first-class.md)      | Prompt caching as a first-class design constraint (static head + dynamic tail) | Accepted |
| [0011](0011-tiered-model-escalation.md)         | Tiered model escalation (start cheap, escalate on need)                        | Accepted |
| [0012](0012-embedding-cache-by-content-hash.md) | Embedding cache keyed by content hash and model version                        | Accepted |
| [0013](0013-tool-use-over-generation.md)        | Prefer tool-use over LLM generation for deterministic subtasks                 | Accepted |
| [0014](0014-lancedb-vector-storage.md)          | LanceDB for vector storage in v1 (supersedes sqlite-vec)                       | Accepted |

## When to write an ADR

A decision earns an ADR when:

- The decision is non-obvious (someone reading the code might wonder why we did this).
- Reversing it would be expensive (architectural, not just preferential).
- Alternatives were genuinely considered.

Decisions that are obvious-in-context, easily reversible, or never had alternatives don't need an ADR.

## When to revise an ADR

ADRs are immutable records of historical decisions. To change a decision:

1. Write a new ADR that describes the new decision.
2. Mark the old ADR as `Superseded by NNNN` at the top.
3. Leave the old ADR's content intact --- it's the historical record.

The aim is to never lose the reasoning that led to a previous state.
