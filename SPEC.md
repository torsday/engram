# engram --- v1 Specification

> Machine-readable v1 acceptance criteria. For full design context see [`docs/design/README.md`](docs/design/README.md). For phasing of v1.1+ see [`docs/design/07-roadmap.md`](docs/design/07-roadmap.md).

## Status

| Field            | Value                                                         |
| ---------------- | ------------------------------------------------------------- |
| Phase            | v1 --- Full personal-use engram                                |
| Implementation   | not started                                                   |
| Acceptance       | not yet met                                                   |
| Target duration  | **~14 months** from start (one developer + Claude Code)        |
| Timeline note    | Originally 3 months → 5--6 months (post-feasibility audit) → ~14 months after the user requested v1 be feature-complete for engram's intended shape. Now absorbs what were previously planned as v1.1 (critical thinking), v1.2 (personal context), and v1.3 (corpus digestion). v2+ remains separately phased (external-facing surfaces, self-improving meta-agents). See `docs/design/07-roadmap.md`. |

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

### Functional --- agents (full personal-use roster)

**Maintenance:**
- [ ] **Linker** proposes wikilinks; high-confidence proposals appear unstaged in `git status`; low-confidence proposals enter `.engram/proposals/`.
- [ ] **Gardener** removes dead links and resolved TODOs; lands unstaged.
- [ ] **Cartographer** maintains `index.md` (continuous mode) and runs quarterly tag audits.
- [ ] **Historian** produces weekly activity-log notes.

**Processing:**
- [ ] **Scribe** cleans fleeting notes (formatting, frontmatter normalization); lands unstaged.
- [ ] **Ingestor** processes PDF, image, plain text, markdown, web URLs, and audio (via local `whisper.cpp`) into literature note drafts.
- [ ] **Inbox Triage** classifies new fleeting notes (keep / promote-literature / promote-evergreen-candidate / merge / discard).
- [ ] **Curator** digests external note corpora into engram (full survey → batch digestion → audit pipeline per `05-corpus-digestion.md`).

**Structural:**
- [ ] **Splitter** proposes specific splits of notes violating atomicity.
- [ ] **Merger** proposes unification of same-concept notes written at different times.
- [ ] **Bridge Builder** finds isolated clusters via community detection; proposes bridge links or notes.

**Thinking:**
- [ ] **Synthesizer** proposes new evergreen notes from clusters of related material.
- [ ] **Devil's Advocate** produces critique; all output passes the Steelman rationality gate.
- [ ] **Steelman** serves both constructive role (strengthen weak notes) AND mandatory rationality-gate role.
- [ ] **Inquirer** generates questions in 4 modes (daily-reactive / seed-empty-note / holistic-gap / blindspot).
- [ ] **Heretic** writes sustained counter-arguments to evergreen notes when a defensible counter-position exists; shelves with "no defensible counter-position found" otherwise.
- [ ] **Confidence Annotator** flags claims lacking explicit confidence markers.
- [ ] **Source Demand** flags factual claims lacking citations.
- [ ] **Pair-Thinking** runs bounded live-writing conversational sessions (3-5 rounds).

**Personal:**
- [ ] **Biographer** maintains `meta/biography.md` (read by other agents to ground their work).
- [ ] **Voice Keeper** learns the user's writing voice; participates in council to flag homogenized prose.
- [ ] **Witness** acknowledges journal/personal notes; on-device only, local-only LLM, no memory.

**Temporal:**
- [ ] **Predictor** maintains predictions ledger AND calibration profile (subsumed Calibration Tracker).
- [ ] **Annual Review** generates yearly long-form narrative reflection notes.

**Pedagogical:**
- [ ] **Tutor** generates spaced-repetition flashcards using FSRS-4.5; daily review session in Swift app.

**Meta:**
- [ ] **Watcher** (basic) — continuous numerical monitoring; trust scores active but not yet modulating thresholds (that's v2.1).
- [ ] **Completion Nudger** surfaces unfinished work as a daily digest.
- [ ] **Backup Watcher** monitors backup recency across configured layers.

### Functional --- council deliberation

- [ ] State machine implemented: `DRAFT → CRITIQUE → REVISE → CONVERGE → {LAND | PROPOSE | SHELVE}`.
- [ ] Per-round, per-participant votes stored in `deliberation_votes`.
- [ ] Quorum selection produces a relevant subset, not the full roster.
- [ ] **Steelman rationality gate** is mandatory for all critical-agent output.
- [ ] Shelved-with-dissent transcripts preserved as vault artifacts.
- [ ] Coordinated flows operational: Evergreen birth ceremony, Daily standup, Insight harvest (basic), Trust ceremony.
- [ ] Flow orchestrator state machine with cost-aware planning (pre-flight estimates, mid-flow checkpoints, user confirmation > $1).

### Functional --- eval framework

- [ ] Each v1 agent has 5-10 seed cases in `.engram/evals/<agent>/cases/`.
- [ ] Quarterly baseline runs execute per agent.
- [ ] Prompt changes trigger an eval run in CI; cannot promote a variant whose scores don't meet or beat the active prompt.
- [ ] Scorecard markdown regenerates after each run with 8-run trend sparklines.

### Functional --- confidence + git safety

- [ ] Every agent action carries a self-assessed `confidence` field (0.0--1.0) in its structured output.
- [ ] Each agent's `auto_land_min_confidence` (per `config.toml`) gates whether the action lands unstaged or becomes a proposal.
- [ ] **No agent ever runs `git add` or `git commit`.** Audit: every row in `agent_actions` shows `git_commit_sha` only when `human_decision = staged`.
- [ ] Every agent write is logged in `agent_actions` with `confidence`, `rationale`, `diff_hash`.
- [ ] User actions (`stage`, `discard`, `amend`) update the corresponding `agent_actions` row.
- [ ] `git restore <path>` discards an agent change; the change does not return without a triggering vault event.

### Functional --- Swift app (capture-first universal app)

- [ ] iOS + macOS universal app from a single Xcode project.
- [ ] **Capture sources:** text, voice memo (local `whisper.cpp` AND cloud Whisper API), share-sheet, document picker, drag-drop (macOS), camera, smart paste, capture batches, voice memo from Apple Watch, lock-screen widget, Action Button binding.
- [ ] **Apple Shortcuts integration** with `capture`, `ask`, `prep`, `untangle` actions.
- [ ] **Offline capture queue** (SwiftData) accepts captures when server is unreachable; sync (idempotent via ULID) when server is reachable; previously-acked captures are no-ops on resend.
- [ ] **Diff review** surface: per-file diff with agent attribution, confidence, and rationale-on-tap. Tap-to-stage runs `git add <path>`; swipe-to-discard runs `git restore <path>`; long-press-to-amend opens an editor.
- [ ] **Bulk actions + keyboard shortcuts** on macOS.
- [ ] **Discard with reason** (hallucinated / wrong direction / redundant / out-of-scope / voice-mismatch) feeds calibration.
- [ ] **Snooze** defers a change for N days.
- [ ] **Search:** calls `/search` API; offline fall-back to local SwiftData FTS index.
- [ ] **Today widget + Lock Screen widget** with pending diffs, due flashcards, today's question, predictions due.
- [ ] **Conversation surface** for Pair-Thinking and Inquirer-conversational-mode; token-streaming via SSE.
- [ ] **Flashcard review session** (Tutor; FSRS-driven swipe interface).
- [ ] **Predictions-due widget** with correct / incorrect / superseded.
- [ ] **Cost dashboard** with month-to-date spend, per-agent breakdown, sparkline.
- [ ] **Backup status indicator.**
- [ ] **Annual Review viewer** (typography-tuned, full-screen, scroll-paced).
- [ ] **Witness inbox** (on-device only; never synced; no share/export buttons).
- [ ] **Spotlight integration** via CoreSpotlight.
- [ ] **Handoff** between iPhone and Mac for capture and diff review.
- [ ] **Universal Clipboard awareness** for capture dedup.
- [ ] **Connection switcher** (v1 ships single-instance; multi-instance + external-MCP client manager land in v2).

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

## Out of v1 (deferred to v2 and beyond)

v1 now covers the full personal-use shape of engram (what was previously split as v1, v1.1, v1.2, v1.3). v2+ remains separately phased for external-facing surfaces and self-improving meta-agents. See [`docs/design/07-roadmap.md`](docs/design/07-roadmap.md) for full phasing.

Notable exclusions from v1:

| Capability                                       | Phase  |
| ------------------------------------------------ | ------ |
| External MCP server + personal-context API       | v2     |
| MCP client manager in Swift app                  | v2     |
| Multi-instance connection switcher in Swift app  | v2     |
| Auditor (quarterly qualitative deep evaluation)  | v2.1   |
| Outcome-based metrics (survival, engagement, downstream productivity) | v2.1 |
| Prompt evolution (shadow-mode A/B variants)      | v2.1   |
| Trust scores modulating confidence thresholds (v1 has trust scores but at fixed-tier behavior) | v2.1 |
| Pacekeeper throttling                            | v2.1   |
| Analogist, Assumption Excavator                  | v2.2   |
| Socratic Prober, Contradiction Detector          | v2.2   |
| Untangler, Research Council, Conversation Prep, Debate Mode | v2.2 |
| Scout (RSS feeds), Fact Checker                  | v2.2   |
| Dream mode, agent spawning, goal-directed sessions | v3+    |
| Cloud relay for external MCP                     | v3+    |
| Multi-user / vault sharing                       | v3+    |

## How to verify v1 acceptance

A future Auditor-style agent (or a manual review by the user) should be able to take this `SPEC.md`, walk every checkbox, and produce a YES/NO/PARTIAL per criterion. The acceptance test for v1 is "every functional and non-functional checkbox is YES, no anti-acceptance item is present, all proxies in 'earned the right to ship more' are met, and the 1-page retro affirms substantive value."
