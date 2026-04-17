# engram --- v1 Specification

> Machine-readable v1 acceptance criteria. For full design context see [`docs/design/README.md`](docs/design/README.md). For phasing of v1.1+ see [`docs/design/07-roadmap.md`](docs/design/07-roadmap.md).

## Status

| Field            | Value                                                         |
| ---------------- | ------------------------------------------------------------- |
| Phase            | v1 --- Foundation                                              |
| Implementation   | not started                                                   |
| Acceptance       | not yet met                                                   |
| Target duration  | **5--6 months** from start (one developer + Claude Code)        |
| Timeline note    | Original 3-month estimate revised after scope-feasibility review found realistic duration is 5--6 months at the chosen scope. See `docs/design/07-roadmap.md` for phase timing. |

## Definition of done for v1

v1 ships when **all** the criteria below are met. Each is a concrete, testable behavior, not an aspiration.

### Functional --- vault and index

- [ ] `engram init <path>` initializes a vault with `.engram/`, `.git/`, and a seeded `welcome.md`.
- [ ] `engram serve` runs against any vault and exposes the REST API on a configurable port (default 7878).
- [ ] File watcher detects vault changes within 2s (debounced) and updates the index.
- [ ] Index rebuilds cleanly via `engram reindex --full`; no functional regression after rebuild.
- [ ] Notes carry frontmatter per [`06-note-conventions.md`](docs/design/06-note-conventions.md): `id` (ULID), `title`, `type`, `status`, `created`, `tags`, `aliases`.
- [ ] Filenames are pure title-slugs (no ID prefix); collisions resolved at write time by appending `-2`, `-3`, etc.
- [ ] Sidecars at `.engram/sidecar/<id>.json` are git-tracked, pretty-printed JSON, schema-versioned.
- [ ] Hybrid retrieval (BM25 + dense + RRF + graph expansion) works against the index. Cross-encoder reranker deferred to v1.1.
- [ ] Stable note IDs survive renames: file move detected via surviving `id:` in frontmatter; link graph updated.

### Functional --- agents (v1 set: 5)

- [ ] **Linker** proposes wikilinks; high-confidence proposals appear unstaged in `git status`; low-confidence proposals enter `.engram/proposals/`.
- [ ] **Gardener** removes dead links and resolved TODOs; lands unstaged.
- [ ] **Cartographer** maintains `index.md`; updates land unstaged.
- [ ] **Scribe** cleans fleeting notes (formatting, frontmatter normalization); lands unstaged.
- [ ] **Ingestor** processes PDF, image, plain text, markdown, and web URLs into literature note drafts.

### Functional --- confidence + git safety

- [ ] Every agent action carries a self-assessed `confidence` field (0.0--1.0) in its structured output.
- [ ] Each agent's `auto_land_min_confidence` (per `config.toml`) gates whether the action lands unstaged or becomes a proposal.
- [ ] **No agent ever runs `git add` or `git commit`.** Audit: every row in `agent_actions` shows `git_commit_sha` only when `human_decision = staged`.
- [ ] Every agent write is logged in `agent_actions` with `confidence`, `rationale`, `diff_hash`.
- [ ] User actions (`stage`, `discard`, `amend`) update the corresponding `agent_actions` row.
- [ ] `git restore <path>` discards an agent change; the change does not return without a triggering vault event.

### Functional --- Swift app (capture-first)

- [ ] iOS + macOS universal app from a single Xcode project.
- [ ] Capture sources working: text, voice memo (cloud Whisper API), share-sheet, document picker, drag-drop (macOS).
- [ ] Offline capture queue (SwiftData) accepts captures when server is unreachable.
- [ ] Captures sync (idempotent via ULID) when server is reachable; previously-acked captures are no-ops on resend.
- [ ] Diff review surface: per-file diff with agent attribution and confidence.
- [ ] Tap-to-stage runs `git add <path>`; swipe-to-discard runs `git restore <path>`; long-press-to-amend opens an editor.
- [ ] Search calls `/search` API; basic results list (no offline FTS index in v1).
- [ ] Cost dashboard visible (month-to-date spend, per-agent breakdown).
- [ ] Connection switcher (single Mac for v1).

### Functional --- internal MCP server

- [ ] stdio transport for Claude Desktop / Code.
- [ ] Tools exposed: `search_notes`, `grep_notes`, `read_note`, `list_tags`, `follow_backlinks`, `recent_changes`, `read_index`.
- [ ] No external MCP server in v1 (deferred to v2).

### Functional --- REST API + CLI

- [ ] REST endpoints: `/notes`, `/notes/:id`, `/search`, `/graph/:id`, `/ingest`, `/changes`, `/changes/:path/{stage,discard,amend}`, `/commit`, `/agents`, `/agents/:name/run`, `/status`, `/events` (SSE).
- [ ] CLI: `engram serve`, `engram reindex`, `engram ingest <file>`, `engram run <agent>`, `engram status`, `engram migrate`, `engram secrets <set|rotate|list>`.

### Non-functional --- performance

All values per [`docs/design/10-performance-budgets.md`](docs/design/10-performance-budgets.md), measured at p95 on a 10K-note vault on Apple Silicon:

- [ ] `/changes` < 50ms
- [ ] `/notes/:id` < 30ms
- [ ] `/search` < 200ms
- [ ] `/ingest` queue ack < 1s (text), < 10s (PDF cloud-vision)
- [ ] Indexer throughput ≥ 100 notes/sec
- [ ] Embedding throughput ≥ 50 notes/sec (local bge-m3 on Apple Silicon)
- [ ] `engram serve` resident memory < 500MB at steady state
- [ ] Swift app cold launch to first interactive < 1s

### Non-functional --- cost

- [ ] System-wide monthly USD cap enforced via [`.engram/config.toml`](docs/design/03-architecture.md); overrun triggers hard pause of LLM-using agents.
- [ ] Per-agent token budgets enforced; overrun pauses the offending agent.
- [ ] Cost-per-landing metric tracked per agent.
- [ ] Default cap: $25/month (configurable up).

### Non-functional --- security

- [ ] Provider API keys stored in macOS Keychain (or Linux Secret Service); never in plaintext config; never committed to git.
- [ ] `engram secrets rotate <provider>` performs an atomic key swap.
- [ ] Privacy zones (`notes/work/`, `notes/medical/`, `notes/journal/`) configured by default; processing routed to local-only models within zones.
- [ ] Per-drop privacy flag in Swift app capture; routes to local-only when set.

### Non-functional --- reliability

- [ ] Backup Watcher monitors configured backup targets and surfaces warnings when stale.
- [ ] Schema migrations apply on `engram serve` startup; documented rollback for migrations that support it.
- [ ] Sidecar schema versioning prevents older binary from corrupting newer sidecars.
- [ ] LLM call failures retry with exponential backoff (3 attempts); on persistent failure, the agent run is marked `failed` and surfaces in Watcher's report.
- [ ] Agent panics caught at the runner level; logged; do not crash `engram serve`.

### Non-functional --- onboarding

- [ ] First-run wizard completes in under 15 minutes for a greenfield vault.
- [ ] Bootstrap mode (first 30 days OR fewer than 100 notes OR fewer than 50 resolved decisions) raises `auto_land_min_confidence` to 0.95 across all agents.
- [ ] Bootstrap-mode notifications include a "what the swarm tried" summary in the standup.
- [ ] Tutorial vault notes tagged `engram/tutorial` so Gardener doesn't propose pruning.

### Operational

- [ ] Single binary (`engram`) builds via `cargo build --release` and runs without external runtime dependencies (modulo Ollama or OpenAI API depending on provider config).
- [ ] Swift app builds for iOS and macOS from a single Xcode project.
- [ ] `task format` runs prettier across the repo without errors.
- [ ] At least one integration test exercises the full ingestion pipeline (drop file → literature note → diff review → stage).
- [ ] At least one integration test verifies the no-agent-commit invariant (run all v1 agents against a sample vault; confirm `git log` shows no commits authored by anyone other than the test harness).

### The "earned the right to ship more" test (falsifiable)

The original framing was "the user prefers engram over plain Obsidian." That's not falsifiable on its own. Concrete proxies, **all** of which must hold over a 30-day post-launch evaluation window:

- [ ] **Capture continuity:** ≥ 30 consecutive days where the user submitted at least one capture via the engram Swift app on the day they captured at all (i.e., no fall-back to alternative tools for capture).
- [ ] **Linker survival:** ≥ 80% of Linker auto-landed proposals are still present (not reverted via `git restore` or removed by user edit) 14 days after they landed. Measured from `agent_actions` rows × outcome data.
- [ ] **Diff-review engagement:** ≥ 70% of pending agent diffs are resolved (staged, discarded, or amended) within 7 days of being proposed. Backlog never exceeds 50 items for more than 72 hours during the evaluation window.
- [ ] **Ingestion through-line:** ≥ 5 distinct external sources (PDF, web URL, image, etc.) ingested via the Ingestor pipeline, surviving as literature notes in the active vault at end of window.
- [ ] **Search usefulness:** ≥ 10 distinct search queries via the Swift app or `engram search` CLI in the 30-day window. (Search is the most basic value-prop; if it's not used, engram is not earning its place.)
- [ ] **Operational stability:** zero data-loss incidents (no markdown or sidecar lost to crash); zero backup-warning escalations beyond a one-day grace window; cost-cap not breached without explicit user override.
- [ ] **Written 1-page retro:** the user writes a 1-page reflection at end of window in `meta/v1-retro.md` answering: "What would I lose if I uninstalled engram tomorrow?" with concrete examples. The retro itself is the falsifiability mechanism — if the answer is "nothing meaningful," v1 has not earned the right to ship more even if all other proxies pass.

## Anti-acceptance --- must NOT be present in v1

- ❌ Any agent running `git add` or `git commit`. (Architecture constraint, type-system enforced per ADR 0003.)
- ❌ Provider API keys in plaintext anywhere on disk.
- ❌ Vault content reachable through external MCP. (External MCP doesn't ship in v1; the server simply isn't started.)
- ❌ Vault content sent to a cloud provider when a privacy zone applies or when the user has flagged a capture private.
- ❌ Free-form chat interface to the vault.
- ❌ Auto-commit of any kind.

## Out of v1 (deferred to later phases)

See [`docs/design/07-roadmap.md`](docs/design/07-roadmap.md) for the full phasing of v1.1 through v3+.

Notable v1 exclusions:

| Capability                                       | Phase  |
| ------------------------------------------------ | ------ |
| Council deliberation engine                      | v1.1   |
| Steelman rationality gate                        | v1.1   |
| Thinking agents (Synthesizer, Devil's Advocate, Heretic, Inquirer, Pair-Thinking) | v1.1 |
| Confidence Annotator + Source Demand             | v1.1   |
| Cross-encoder reranker in retrieval              | v1.1   |
| Today widget + on-demand quick-launch buttons    | v1.1   |
| Apple Shortcuts integration                      | v1.1   |
| Personal agents (Biographer, Voice Keeper, Witness) | v1.2 |
| Predictor + calibration                          | v1.2   |
| Annual Review                                    | v1.2   |
| Tutor (spaced-repetition flashcards)             | v1.2   |
| Local audio transcription (`whisper.cpp`)         | v1.2   |
| Apple Watch capture app                          | v1.2   |
| Curator + corpus digestion                       | v1.3   |
| Inbox Triage                                     | v1.3   |
| Splitter / Merger / Bridge Builder               | v1.3   |
| External MCP server + personal-context API       | v2     |
| Auditor + outcome metrics + prompt evolution     | v2.1   |
| Trust scores                                     | v2.1   |
| Pacekeeper                                       | v2.1   |
| Analogist, Assumption Excavator, Socratic Prober, Contradiction Detector, Untangler, Research Council, Conversation Prep, Scout, Fact Checker | v2.2 |
| Coordinated flows (evergreen birth ceremony, daily standup, insight harvest, trust ceremony) | v2.2 |
| Dream mode, agent spawning, goal-directed sessions, cloud relay, multi-user | v3+ |

## How to verify v1 acceptance

A future Auditor-style agent (or a manual review by the user) should be able to take this `SPEC.md`, walk every checkbox, and produce a YES/NO/PARTIAL per criterion. The acceptance test for v1 is "every functional and non-functional checkbox is YES, no anti-acceptance item is present, all proxies in 'earned the right to ship more' are met, and the 1-page retro affirms substantive value."
