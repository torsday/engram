# engram — Design Corpus

> Your thoughts, encoded. A living knowledge base that rewrites itself.

This directory holds the complete design for engram. **No code yet** — the system is in pre-implementation design phase. The goal of this corpus is that any reader (the original author six months from now, a contributor, a future implementer, or Claude Code starting work) can pick up engram's vision and architecture without external context.

## Status

Design complete and ready for v1 implementation. v1 scope is explicit (see `07-roadmap.md`); v1 acceptance criteria are machine-readable in [`/SPEC.md`](../../SPEC.md). **Thirteen ADRs** capture the load-bearing architectural decisions, including the four-ADR token-efficiency stack (prompt caching, tier escalation, embedding cache, tool-use over generation). The agent spec template ([12](12-agent-spec-template.md)) is filled for the five v1 agents. The testing strategy ([13](13-testing-strategy.md)) defines how every layer of the system is verified. The eval framework + cost-aware planning (in [01](01-agents-and-council.md)) define how agents get systematically better and stay within budget. Streaming + request coalescing (in [03](03-architecture.md)) provide additional latency and cost wins. Open questions are flagged within the relevant docs and at the end of `00-overview.md`.

## Reading order

If reading the corpus for the first time:

1. **[00-overview.md](00-overview.md)** — vision, principles, surfaces, note-type hierarchy, evergreen rubric, review-and-agency model. **Read first.**
2. **[07-roadmap.md](07-roadmap.md)** — v1 scope, phasing, anti-scope. Read second to understand what's actually being built first.
3. **[06-note-conventions.md](06-note-conventions.md)** — how notes are shaped (filenames, frontmatter, sidecar, folders, tags, wikilinks, provenance). The dual-citizen design pattern.
4. **[01-agents-and-council.md](01-agents-and-council.md)** — full agent roster, rationality gate, council deliberation, coordinated flows, system-level features (memory, trust, prompt evolution, dream mode, sessions).
5. **[03-architecture.md](03-architecture.md)** — Rust workspace, Swift app, SQLite schemas, REST + SSE API, MCP servers, retrieval pipeline, concurrency model, backup, secrets, schema migration, system-wide cost cap.
6. **[02-ingestion.md](02-ingestion.md)** — drop-to-literature-note pipeline, content-addressed artifact storage, privacy routing, batch ingestion.
7. **[05-corpus-digestion.md](05-corpus-digestion.md)** — Curator agent and pipeline for digesting external corpora (e.g., an old Obsidian vault) into engram with aggressive curation.
8. **[04-external-mcp.md](04-external-mcp.md)** — scoped + authenticated MCP server so the user's other apps can read personal context and write back.
9. **[08-first-run.md](08-first-run.md)** — onboarding wizard, bootstrap mode, sparse-content handling for context agents.
10. **[09-threat-model.md](09-threat-model.md)** — what engram defends against and what it explicitly doesn't.
11. **[10-performance-budgets.md](10-performance-budgets.md)** — quantitative targets for indexer throughput, query latency, agent timing, cost.
12. **[11-scenarios.md](11-scenarios.md)** — day-in-the-life walkthroughs grounding the design in real interactions.
13. **[12-agent-spec-template.md](12-agent-spec-template.md)** — the one-page agent specification template, filled for the five v1 agents (Linker, Gardener, Cartographer, Scribe, Ingestor); includes the v1 proposal-without-council file format.
14. **[13-testing-strategy.md](13-testing-strategy.md)** — testing approach: unit, property, snapshot, integration, end-to-end. Mock LLM provider. The no-agent-commit invariant test.

Then dive into the [ADRs](adrs/) for the *why* behind major architectural choices.

## Quick index by topic

| Topic | Where to look |
|---|---|
| **What is engram?** | [00-overview.md](00-overview.md) — vision and principles |
| **What's in v1?** | [07-roadmap.md](07-roadmap.md) — explicit scope |
| **The agent roster** | [01-agents-and-council.md](01-agents-and-council.md) |
| **How agents stay safe** | [01-agents-and-council.md](01-agents-and-council.md) — confidence-gated autonomy + git safety; [ADR 0003](adrs/0003-no-agent-commits.md), [ADR 0004](adrs/0004-confidence-gated-autonomy.md), [ADR 0009](adrs/0009-git-read-write-boundary.md) |
| **How agents are specified** | [12-agent-spec-template.md](12-agent-spec-template.md) — the spec template + 5 v1 fills |
| **How agents get systematically better** | [01-agents-and-council.md](01-agents-and-council.md) — Eval framework section |
| **How agents stay token-efficient** | [ADR 0010](adrs/0010-prompt-caching-first-class.md) (prompt caching), [ADR 0011](adrs/0011-tiered-model-escalation.md) (tier escalation), [ADR 0012](adrs/0012-embedding-cache-by-content-hash.md) (embedding cache), [ADR 0013](adrs/0013-tool-use-over-generation.md) (tool-use over generation), plus streaming + early-exit + request coalescing in `03-architecture.md`, plus cost-aware planning in `01-agents-and-council.md` |
| **Testing approach** | [13-testing-strategy.md](13-testing-strategy.md) |
| **How critique stays rigorous** | [01-agents-and-council.md](01-agents-and-council.md) — rationality gate; [ADR 0007](adrs/0007-steelman-rationality-gate.md) |
| **File and metadata conventions** | [06-note-conventions.md](06-note-conventions.md); [ADR 0005](adrs/0005-sidecar-json.md), [ADR 0006](adrs/0006-pure-title-slug-filenames.md) |
| **Tech stack** | [03-architecture.md](03-architecture.md) — tech-stack table; [ADR 0001](adrs/0001-rust-everywhere.md) |
| **The Swift app** | [03-architecture.md](03-architecture.md) — Swift app section |
| **Ingesting external content** | [02-ingestion.md](02-ingestion.md) (single files) and [05-corpus-digestion.md](05-corpus-digestion.md) (whole corpora) |
| **Personal-context API for other apps** | [04-external-mcp.md](04-external-mcp.md); [ADR 0008](adrs/0008-two-mcp-servers.md) |
| **Backup, secrets, migrations, cost** | [03-architecture.md](03-architecture.md) — late sections |
| **Onboarding a new vault** | [08-first-run.md](08-first-run.md) |
| **Security posture** | [09-threat-model.md](09-threat-model.md) |
| **Performance targets** | [10-performance-budgets.md](10-performance-budgets.md) |
| **What it feels like to use** | [11-scenarios.md](11-scenarios.md) |
| **Term lookup** | [glossary.md](glossary.md) |
| **Why it's this way** | [adrs/](adrs/) |

## Document map

```
docs/design/
├── README.md                       (this file)
├── 00-overview.md                  vision, principles, surfaces, note types, review model
├── 01-agents-and-council.md        agent roster, council, rationality gate, flows, system features
├── 02-ingestion.md                 drop-to-literature-note pipeline
├── 03-architecture.md              tech, crates, Swift app, schema, API, MCP, backup, secrets
├── 04-external-mcp.md              scoped MCP for the user's other apps; personal-context API
├── 05-corpus-digestion.md          Curator and the pipeline for digesting external corpora
├── 06-note-conventions.md          filenames, frontmatter, sidecar, folders, tags, provenance
├── 07-roadmap.md                   v1 scope decision and phasing
├── 08-first-run.md                 onboarding wizard, bootstrap mode
├── 09-threat-model.md              what we defend against and what we don't
├── 10-performance-budgets.md       quantitative targets
├── 11-scenarios.md                 day-in-the-life walkthroughs
├── 12-agent-spec-template.md       agent spec template + 5 v1 agent specs + v1 proposal format
├── 13-testing-strategy.md          testing approach (unit, property, snapshot, integration, e2e)
├── glossary.md                     vocabulary index
└── adrs/
    ├── README.md                   ADR index
    ├── 0001-rust-everywhere.md
    ├── 0002-agents-as-data.md
    ├── 0003-no-agent-commits.md
    ├── 0004-confidence-gated-autonomy.md
    ├── 0005-sidecar-json.md
    ├── 0006-pure-title-slug-filenames.md
    ├── 0007-steelman-rationality-gate.md
    ├── 0008-two-mcp-servers.md
    ├── 0009-git-read-write-boundary.md
    ├── 0010-prompt-caching-first-class.md
    ├── 0011-tiered-model-escalation.md
    ├── 0012-embedding-cache-by-content-hash.md
    └── 0013-tool-use-over-generation.md
```

## Conventions used in the docs

- **Code blocks** show types, schemas, examples. Many are Rust, TOML, JSON, YAML, and SQL.
- **Cross-references** use relative markdown links: `[03-architecture.md](03-architecture.md)`.
- **ADR references** look like `[ADR 0003](adrs/0003-no-agent-commits.md)`.
- **Numbered docs (00--11)** are sequenced; the numbering reflects reading order, not strict dependency.
- **Open questions** are called out explicitly, usually with the word "Open" in a heading or bolded inline.

## Provenance of this design

The corpus was developed iteratively in design conversation, drawing on:

- Andy Matuschak's [evergreen notes](https://notes.andymatuschak.org/Evergreen_notes) framework
- Maggie Appleton's [Language Model Sketchbook](https://maggieappleton.com/lm-sketchbook) (daemons, epi, branches; "epistemic rubber ducks")
- Andrej Karpathy's [llm-wiki gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) (the index.md navigation pattern)
- The Zettelkasten tradition (atomicity, density, source separation)
- LLM-RAG benchmark evidence circa 2026 (hybrid search + rerank + graph expansion)
- Anthropic's MCP protocol (the delivery mechanism for personal context)

The synthesis is engram-specific. The design does not strictly follow any single source — it takes what works and discards what doesn't.
