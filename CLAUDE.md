# CLAUDE.md

This is **engram** --- your thoughts, encoded. A living knowledge base that rewrites itself.

## Status

**v1 implementation in flight.** The Rust workspace at `crates/` holds the production code:

- `engram-core` --- configuration, schemas, shared error categories
- `engram-agents` --- agent runner + scheduler + per-agent typed output schemas (`src/agents/<name>.rs`)
- `engram-llm` --- provider trait + Anthropic/OpenAI/Ollama implementations + resilience stack (timeout/retry/circuit-breaker)
- `engram-index` --- SQLite (FTS5) metadata index + migrations
- `engram-eval` --- eval framework (cases, scoring, scorecard, regression gate)
- `engram-cli` --- the `engram` binary (`serve`, `eval`, `agents`, `backup`, `migrate`, `config`, ...)
- `engram-mcp` --- MCP server (stdio + HTTP+SSE per ADR 0008)
- `engram-api` --- HTTP+SSE surface
- `engram-extract` --- file ingestion + extraction
- `engram-git` --- git read/write boundary per ADR 0009

The 9 v1 agent prompts + configs live at `agents/<name>/{config.toml, prompt.md}` (Steelman constructive, Devil's Advocate, Inquirer, Synthesizer, Voice Keeper, Pair-Thinking, Splitter, Merger, Bridge Builder).

**What's not yet wired:** runner integration of the typed output structs (today the runner extracts confidence permissively via `serde_json::Value::get`; the typed `validate()` dispatch is an opt-in strict surface for eval cases, CLI, and future runner work --- see [ADR 0016](docs/design/adrs/0016-per-agent-typed-outputs.md)). The Swift universal app, REST+SSE API surface, council deliberation engine, and several whole-agent pipelines (Ingestor, Curator) are blocked on upstream slices --- see [`docs/design/07-roadmap.md`](docs/design/07-roadmap.md) for the current phasing.

**Licensing:** All rights reserved. There is no `LICENSE` file by intent --- engram is private-by-default and the user has not yet decided whether to open-source it. Do not assume permissive use.

## Where to look for what

- **Adding or modifying an agent:** the four-layer stack documented in [ADR 0016](docs/design/adrs/0016-per-agent-typed-outputs.md) --- `agents/<name>/{config.toml, prompt.md}` (slice 1), `crates/engram-agents/src/agents/<name>.rs` (slice 2 typed output), `crates/engram-agents/src/agents/validate.rs` (dispatch registry), `tests/fixtures/agents/<name>/output/*.json` (exemplars). The integration test `crates/engram-agents/tests/fixture_outputs.rs` walks the fixtures through `validate()` at CI time and surfaces drift between any two layers.
- **Validating a captured agent output:** `engram agents list` enumerates the registered agents; `engram agents validate <name> --file <path>` (or `--file -` from stdin, or `--all` to walk every fixture) schema-checks responses against the typed Rust struct.
- **Running an eval suite:** `engram eval <agent>` reads `.engram/evals/<agent>/cases/` and writes a scorecard; `--baseline <path>` adds the 5%-regression CI gate.

## Where to start

1. Read [`docs/design/README.md`](docs/design/README.md) first --- it indexes the full design corpus (14 numbered docs + 14 ADRs + glossary) and gives a reading order.
2. For v1 scope and phasing, see [`docs/design/07-roadmap.md`](docs/design/07-roadmap.md).
3. For machine-readable v1 acceptance criteria, see [`SPEC.md`](SPEC.md).
4. For non-obvious architectural decisions and the reasoning behind them, see [`docs/design/adrs/`](docs/design/adrs/).
5. For implementing any of the 5 v1 agents, see [`docs/design/12-agent-spec-template.md`](docs/design/12-agent-spec-template.md) --- prompts, schemas, confidence formulas, and tools are all specified.
6. For testing standards (unit, property, snapshot, integration, e2e, mock LLM), see [`docs/design/13-testing-strategy.md`](docs/design/13-testing-strategy.md).

## Standing rules (apply to every session)

These are personal rules from the user (also encoded in `~/.claude/CLAUDE.md`):

- **Never commit, stage, unstage, or push** unless I explicitly ask.
- Work on whatever branch I am on. Don't create branches without asking.
- Follow Conventional Commits (`type(scope): subject` --- imperative, lowercase, no trailing period).
- Do not add `Co-Authored-By` footers (or any AI-attribution footer) to commit messages.
- Use the `commit` skill when I ask you to commit.
- Use the `adr` skill when documenting a new architectural decision.

## Tech direction (per design)

- **Rust** for the entire core --- no Python or TypeScript agent layer (see [ADR 0001](docs/design/adrs/0001-rust-everywhere.md)).
- **SwiftUI** universal app for iOS + macOS.
- **Vault is plain markdown + git;** SQLite (FTS5) for derived metadata index; LanceDB (per [ADR 0014](docs/design/adrs/0014-lancedb-vector-storage.md)) for vector search.
- **Agents never run `git add` or `git commit`** --- all agent writes are unstaged; the human is the only entity that touches git history (see [ADR 0003](docs/design/adrs/0003-no-agent-commits.md)).
- **Confidence-gated autonomy** --- agents self-assess confidence; below threshold, propose; above, write unstaged (see [ADR 0004](docs/design/adrs/0004-confidence-gated-autonomy.md)).
- **Layered metadata** --- lean human-readable frontmatter + rich agent-readable sidecar JSON in `.engram/sidecar/<id>.json` (see [ADR 0005](docs/design/adrs/0005-sidecar-json.md)).
- **Pure title-slug filenames** --- ULID lives in frontmatter only (see [ADR 0006](docs/design/adrs/0006-pure-title-slug-filenames.md)).
- **Steelman is the rationality gate** for all critical agents --- contrarianism for its own sake is rejected (see [ADR 0007](docs/design/adrs/0007-steelman-rationality-gate.md)).
- **Token efficiency is architectural**, not an afterthought --- four ADRs together: prompts structured for caching ([ADR 0010](docs/design/adrs/0010-prompt-caching-first-class.md)), agents start cheap and escalate only on need ([ADR 0011](docs/design/adrs/0011-tiered-model-escalation.md)), embeddings cached by content hash ([ADR 0012](docs/design/adrs/0012-embedding-cache-by-content-hash.md)), and tool-use is preferred over LLM generation for deterministic subtasks ([ADR 0013](docs/design/adrs/0013-tool-use-over-generation.md)). Plus streaming + early-exit + request coalescing in `docs/design/03-architecture.md`.

## Doc conventions

- Numbered docs (`00-`, `01-`, ..., `11-`) live in `docs/design/`. Adding a new design doc follows the next number.
- ADRs live in `docs/design/adrs/` as `NNNN-decision-name.md` and follow the template in `adrs/README.md` (Status, Context, Decision, Alternatives, Consequences, References).
- Markdown formatting: use `task format` (prettier) before committing markdown.
- Cross-doc links use relative paths.

## What this repo is NOT

- Not a fork of Obsidian. Engram is a layer over an Obsidian vault, not a replacement for it.
- Not a chatbot interface for notes. Research Council, Untangler, and Pair-Thinking are bounded structured surfaces, not chat.
- Not a SaaS. Single-user only. v3+ may add a cloud relay for personal access from anywhere; never multi-tenant hosting.
- Not currently building anything externally --- this is a personal tool by Torsday.

## When unsure

Read the relevant design doc. If a question is genuinely ambiguous and you can't tell from the docs, ask the user before guessing.
