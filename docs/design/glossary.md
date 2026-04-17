# Glossary

Terms used across the engram design corpus, alphabetically. Each entry is a one-line definition plus a pointer to the doc that defines it in detail.

## A

**Action log** — The `agent_actions` SQLite table that records every unstaged write by every agent, regardless of how the user later groups them into commits. The primary audit trail. → [03-architecture.md](03-architecture.md)

**Agent spec template** — The one-page format every agent fills before implementation: identity, prompt skeleton, structured output schema, confidence formula, tools, triggers, outputs, test fixtures. → [12-agent-spec-template.md](12-agent-spec-template.md)

**ADR (Architecture Decision Record)** — A short document capturing the reasoning behind a load-bearing architectural choice. → [adrs/README.md](adrs/README.md)

**Agent** — A specialized role in the system, defined as a directory of `prompt.md` + `config.toml` (and optional `tools.toml`). Hot-reloaded. → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0002](adrs/0002-agents-as-data.md)

**Agent action** — A single proposed change by an agent. Logged in `agent_actions` with confidence, rationale, diff hash, and human resolution. → [01-agents-and-council.md](01-agents-and-council.md)

**Agent memory** — Per-agent persistent key-value store in SQLite for cross-run state (rejection history, hypothesis tracking, conversation continuity). → [01-agents-and-council.md](01-agents-and-council.md)

**Agent spawning** — A speculative v3+ feature where the swarm proposes new agents based on observed gaps in coverage. → [01-agents-and-council.md](01-agents-and-council.md)

**Analogist** — Agent that finds structural parallels between ideas in different domains. → [01-agents-and-council.md](01-agents-and-council.md)

**Annual Review** — Yearly long-form narrative reflection agent. → [01-agents-and-council.md](01-agents-and-council.md)

**Archive (note type)** — Verbatim-preserved content from corpus digestion. Read-only; agents skip these (only Cartographer indexes them). → [05-corpus-digestion.md](05-corpus-digestion.md)

**Artifact** — A raw ingested file (PDF, image, audio, etc.), stored content-addressed by SHA-256 in `.engram/artifacts/`. → [02-ingestion.md](02-ingestion.md)

**Assumption Excavator** — Agent that surfaces unstated premises in evergreen notes. → [01-agents-and-council.md](01-agents-and-council.md)

**Auditor** — Meta-agent that performs deep qualitative evaluation of every agent quarterly. Distinct from Watcher (which counts continuously). → [01-agents-and-council.md](01-agents-and-council.md)

**Auto-land** — A change written to the working tree (unstaged) without going through council. Gated by confidence threshold and invasiveness ceiling. Agents never `git add`. → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0003](adrs/0003-no-agent-commits.md)

## B

**Backup Watcher** — Agent that monitors backup recency across configured layers (git remote, filesystem snapshots, artifact remote). Does not perform backups. → [03-architecture.md](03-architecture.md)

**Biographer** — Agent that maintains the system's evolving model of who the user is. Read by other agents to ground their work. → [01-agents-and-council.md](01-agents-and-council.md)

**Block ID** — Obsidian's `^block-id` syntax for referencing a specific paragraph or sentence. Used by agents for precise quoting. → [06-note-conventions.md](06-note-conventions.md)

**Bootstrap mode** — The first 30 days (or first 100 notes) of a new vault, during which auto-land thresholds are stricter (0.95) and trust scores are inactive. → [08-first-run.md](08-first-run.md)

**Circuit breaker** — Per-provider state machine (closed/open/half-open) that fails fast when a provider is down to prevent cascading retries from burning budget. → [03-architecture.md](03-architecture.md)

**Cost-aware planning** — Pre-flight cost estimation for multi-step flows (Curator batches, Annual Review, Research Council). User confirms flows estimated > $1; mid-flow checkpoint pauses if actual exceeds estimate by >50%. → [01-agents-and-council.md](01-agents-and-council.md)

**Embedding cache** — A SQLite cache keyed by `(content_hash, embedding_model, embedding_dim)`. Re-embedding only happens on actual content change; switching models doesn't blow away old vectors. ~95% cost reduction on stable vaults. → [ADR 0012](adrs/0012-embedding-cache-by-content-hash.md)

**Early-exit (streaming)** — Agent calls stream structured JSON; the runner parses incrementally and cancels mid-stream when `confidence` arrives below the agent's `early_exit_confidence_floor`. Saves output tokens on rejected calls. → [03-architecture.md](03-architecture.md)

**Bridge Builder** — Structural agent that finds and connects isolated graph clusters. → [01-agents-and-council.md](01-agents-and-council.md)

## C

**Calibration** — How well an agent's claimed confidence matches actual acceptance rate. Watcher tracks; prompt evolution tunes. → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0004](adrs/0004-confidence-gated-autonomy.md)

**Cartographer** — Maintenance agent for MOCs, indices, navigation, and the tag taxonomy (continuous mode + quarterly audit mode; subsumed the formerly-separate Taxonomist). → [01-agents-and-council.md](01-agents-and-council.md)

**Confidence threshold** — Per-agent value (default 0.85) above which the agent writes autonomously, below which the action becomes a council proposal. → [01-agents-and-council.md](01-agents-and-council.md)

**Confidence-gated autonomy** — The principle that every agent action is gated by self-assessed confidence, not just by trust score classification. → [00-overview.md](00-overview.md), [ADR 0004](adrs/0004-confidence-gated-autonomy.md)

**Confidence Annotator** — Agent that flags claims in evergreen notes lacking explicit confidence markers. → [01-agents-and-council.md](01-agents-and-council.md)

**Contradiction Detector** — Agent that finds claims conflicting across notes (where the user disagrees with their past self). → [01-agents-and-council.md](01-agents-and-council.md)

**Conversation Prep** — On-demand agent that produces briefings before meetings or conversations. Calendar integration. → [01-agents-and-council.md](01-agents-and-council.md)

**Conversational agent** — An agent that participates in bounded back-and-forth dialogues with the user via the Swift app (Pair-Thinking, Socratic Prober, etc.). → [01-agents-and-council.md](01-agents-and-council.md)

**Council** — The deliberation engine that runs when agents propose changes above their auto-land thresholds. State machine: DRAFT → CRITIQUE → REVISE → CONVERGE. → [01-agents-and-council.md](01-agents-and-council.md)

**Council deliberation** — A multi-agent discussion of a proposed change, producing a deliberation transcript. → [01-agents-and-council.md](01-agents-and-council.md)

**Curator** — Processing agent for digesting external note corpora into engram. Distinct from Ingestor (single files). → [05-corpus-digestion.md](05-corpus-digestion.md)

## D

**Daily standup** — A morning report in the Swift app summarizing what the swarm did overnight and what needs attention today. → [01-agents-and-council.md](01-agents-and-council.md)

**Deliberation** — A council session and its transcript. Stored in `.engram/deliberations/`. → [01-agents-and-council.md](01-agents-and-council.md)

**Devil's Advocate** — Critical agent that argues against claims. Output gated by Steelman. → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0007](adrs/0007-steelman-rationality-gate.md)

**Diff review** — The Swift app's primary review surface: unstaged `git diff` per file with agent attribution and confidence. Tap-to-stage, swipe-to-discard. → [03-architecture.md](03-architecture.md)

**Disposition** — In corpus digestion, the per-note decision of what to do with a source: keep-evergreen-draft, keep-literature, merge-into, archive, discard, or defer. → [05-corpus-digestion.md](05-corpus-digestion.md)

**Dream mode** — A speculative v3+ idle-time process producing low-confidence speculative outputs in `.engram/dreams/`. → [01-agents-and-council.md](01-agents-and-council.md)

## E

**engram** — The system itself. The name nods to memory traces in neuroscience.

**Evergreen birth ceremony** — The coordinated multi-agent flow that runs when a note is being promoted to `status: evergreen`. → [01-agents-and-council.md](01-agents-and-council.md)

**Evergreen note** — A curated, atomic, concept-oriented, densely-linked note. The vault's core. Per Andy Matuschak's framework. → [00-overview.md](00-overview.md)

**Eval framework** — Per-agent benchmark suite of held-out test cases. Run before promoting prompt-evolution variants; quarterly baseline; CI gate on prompt changes. The mechanism for systematic agent improvement. → [01-agents-and-council.md](01-agents-and-council.md)

**External MCP** — The HTTP+SSE-transport, scope-authenticated MCP server exposing engram to the user's other apps. → [04-external-mcp.md](04-external-mcp.md), [ADR 0008](adrs/0008-two-mcp-servers.md)

**Flow orchestrator** — The shared state machine that runs coordinated flows (evergreen birth ceremony, trust ceremony, insight harvest). State persisted in `flow_runs`. Resumable on user input or after pause. → [01-agents-and-council.md](01-agents-and-council.md)

**FSRS** — Free Spaced Repetition Scheduler (FSRS-4.5), the algorithm Tutor uses to schedule flashcard reviews. → [01-agents-and-council.md](01-agents-and-council.md)

## F

**Fact Checker** — External agent that verifies claims against current external sources. → [01-agents-and-council.md](01-agents-and-council.md)

**First-run** — The wizard and bootstrap behavior on a fresh vault. → [08-first-run.md](08-first-run.md)

**Fleeting note** — A quick-capture note (voice memo, share-sheet drop, typed thought). Type `fleeting`. → [00-overview.md](00-overview.md)

**Frontmatter (lean)** — The minimal YAML at the top of a note: `id`, `title`, `type`, `status`, `created`, `tags`, `aliases`. Rich agent metadata lives in the sidecar. → [06-note-conventions.md](06-note-conventions.md), [ADR 0005](adrs/0005-sidecar-json.md)

## G

**Gardener** — Maintenance agent for pruning stale content, fixing dead links, removing resolved TODOs. → [01-agents-and-council.md](01-agents-and-council.md)

**Goal-directed session** — A speculative v3+ feature where a temporary agent constellation forms around a research goal. → [01-agents-and-council.md](01-agents-and-council.md)

## H

**Heretic** — Critical agent that periodically writes sustained counter-arguments to evergreen notes. Output gated by Steelman; shelves with "no defensible counter-position found" when no rational opposition exists. → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0007](adrs/0007-steelman-rationality-gate.md)

**Heretical note** — A note (`type: heretical`) that argues against another note. Created by the Heretic agent. → [01-agents-and-council.md](01-agents-and-council.md)

**Historian** — Maintenance agent that produces weekly activity-log notes. → [01-agents-and-council.md](01-agents-and-council.md)

## I

**Index (Karpathy-style)** — A single `index.md` with one-line summaries of every note, maintained by Cartographer. Sometimes outperforms vector search for navigation queries. → [03-architecture.md](03-architecture.md)

**Ingestor** — Processing agent that turns dropped files into literature notes via the extraction pipeline. → [02-ingestion.md](02-ingestion.md)

**Inbox Triage** — Processing agent that classifies new fleeting notes (keep-fleeting, promote-literature, promote-evergreen-candidate, merge, discard). → [01-agents-and-council.md](01-agents-and-council.md)

**Inquirer** — 4-mode question-generation agent (consolidates the formerly-separate Interlocutor, Prompt Drafter, Question Generator, and Blindspot Finder). Modes: daily-reactive, seed-empty-note, holistic-gap, blindspot. → [01-agents-and-council.md](01-agents-and-council.md)

**Insight harvest** — Quarterly flow that scans generative agent outputs to learn which kinds of work the user actually used. Feeds prompt evolution. → [01-agents-and-council.md](01-agents-and-council.md)

**Internal MCP** — The stdio-transport, full-access MCP server for Claude Desktop on the user's machine. → [03-architecture.md](03-architecture.md), [ADR 0008](adrs/0008-two-mcp-servers.md)

**Invasiveness** — A change's class: mechanical, additive, editorial, or structural. Each agent has a `max_invasiveness` ceiling. → [01-agents-and-council.md](01-agents-and-council.md)

## L

**Layered metadata** — The principle that human-relevant fields live in frontmatter while agent-rich metadata lives in the sidecar JSON. → [06-note-conventions.md](06-note-conventions.md), [ADR 0005](adrs/0005-sidecar-json.md)

**Linker** — Maintenance agent that proposes wikilinks between notes. → [01-agents-and-council.md](01-agents-and-council.md)

**Literature note** — A note (`type: literature`) representing one external source. Holds summary, key claims, citation, and a reference to the raw artifact. → [02-ingestion.md](02-ingestion.md)

## M

**MCP (Model Context Protocol)** — Anthropic's protocol for exposing tools to LLM clients. Engram runs two MCP servers. → [03-architecture.md](03-architecture.md), [04-external-mcp.md](04-external-mcp.md)

**MOC (Map of Content)** — A note (`type: moc`) that organizes navigation by topic or theme. Maintained by Cartographer. → [00-overview.md](00-overview.md)

## N

**Note conventions** — The dual-citizen design (human-first markdown + agent-first sidecar) for how notes are structured. → [06-note-conventions.md](06-note-conventions.md)

## O

**Outcome metric** — Beyond accept/reject, the longitudinal signal of whether an agent's change was actually valuable: survival, engagement, downstream productivity, reversal. → [01-agents-and-council.md](01-agents-and-council.md)

## P

**Pacekeeper** — Meta-agent that throttles producing agents when the user's diff queue grows faster than they can review. → [01-agents-and-council.md](01-agents-and-council.md)

**Pair-Thinking** — Conversational agent that collaborates during live writing. Bounded session of 3--5 rounds. → [01-agents-and-council.md](01-agents-and-council.md)

**Personal context** — A structured digest combining Biographer model + relevant notes + trajectory + recent thinking + preferences, packaged for an external client's LLM. The headline external-MCP tool. → [04-external-mcp.md](04-external-mcp.md)

**Predictor** — Temporal agent that maintains a prediction ledger and computes calibration over resolved predictions (subsumes the formerly-separate Calibration Tracker). → [01-agents-and-council.md](01-agents-and-council.md)

**Privacy zone** — A vault path prefix (e.g., `notes/work/`, `notes/medical/`) that is excluded from external MCP and routed to local-only processing by default. → [02-ingestion.md](02-ingestion.md), [04-external-mcp.md](04-external-mcp.md)

**Prompt evolution** — The lightweight RLHF-like loop where agent prompt variants run in shadow mode, are compared on outcome metrics, and Auditor proposes promotion. → [01-agents-and-council.md](01-agents-and-council.md)

**Pacekeeper state** — One of `normal`, `throttled`, `paused`. Computed hourly from backlog signals. Affects agent thresholds and which agents are deferred. State file at `.engram/meta/pace.md`. → [01-agents-and-council.md](01-agents-and-council.md)

**Prompt caching (static head + dynamic tail)** — Engram structures every agent prompt with a cacheable static prefix (instructions, schema, biographer model) and a non-cacheable dynamic suffix (note context, retrieval results). Reduces input cost ~10× for frequently-running agents. → [ADR 0010](adrs/0010-prompt-caching-first-class.md)

**Request coalescing** — Identical retrieval requests (e.g., five council members all reading the same note) within a short window (50-200ms) share a single fetch. Reduces both latency and cost during council activity. → [03-architecture.md](03-architecture.md)

**Pending question** — A question routed from an external MCP client to the user via `ask_user`. Lives in `pending_questions` table. User replies, skips, or mutes the asking app. → [04-external-mcp.md](04-external-mcp.md)

**Proposal** — In v1, a JSON file at `.engram/proposals/<id>.json` representing a proposed change awaiting human review. The Swift app surfaces proposals for stage/discard/edit. In v1.1+, proposals also flow through council deliberation before reaching the queue. → [12-agent-spec-template.md](12-agent-spec-template.md)

**Provenance** — The complete record of who wrote what, when, and under what deliberation. Three layers: action log (sqlite), block-level HTML comments (in note), sidecar history (JSON). → [01-agents-and-council.md](01-agents-and-council.md), [06-note-conventions.md](06-note-conventions.md)

## R

**Rationality gate** — The five-criteria check Steelman applies to all critique before it can land. → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0007](adrs/0007-steelman-rationality-gate.md)

**ReadOnlyGit / WriteGit** — The two trait types in `engram-git` that enforce the no-agent-commit invariant at compile time. Agents receive only `ReadOnlyGit`; mutations go through `WriteGit`, available only to user-invoked HTTP/CLI handlers. → [ADR 0009](adrs/0009-git-read-write-boundary.md)

**Research Council** — On-demand agent that accepts a question and produces a structured briefing note from the vault. → [01-agents-and-council.md](01-agents-and-council.md)

**Review queue** — The Swift app's diff-review surface (unstaged changes) and proposals queue (high-invasiveness changes awaiting explicit approval). → [03-architecture.md](03-architecture.md)

## S

**Scope (MCP)** — An OAuth-style permission string granted to an external MCP client (`personal_context:read`, `notes:write:type/literature`, etc.). → [04-external-mcp.md](04-external-mcp.md)

**Scout** — External agent that monitors RSS/feeds for relevant content. → [01-agents-and-council.md](01-agents-and-council.md)

**Scribe** — Processing agent for cleaning up fleeting notes and formatting literature notes. → [01-agents-and-council.md](01-agents-and-council.md)

**Sidecar JSON** — Per-note rich metadata file at `.engram/sidecar/<id>.json`. Git-tracked, pretty JSON. → [06-note-conventions.md](06-note-conventions.md), [ADR 0005](adrs/0005-sidecar-json.md)

**Slug (filename)** — A note's filename, derived from its title (lowercase, hyphenated, punctuation stripped). Pure title-slug, no ID prefix. → [06-note-conventions.md](06-note-conventions.md), [ADR 0006](adrs/0006-pure-title-slug-filenames.md)

**Socratic Prober** — Critical agent that stress-tests notes before they earn `status: evergreen`. → [01-agents-and-council.md](01-agents-and-council.md)

**Source Demand** — Agent that flags factual claims lacking citations. → [01-agents-and-council.md](01-agents-and-council.md)

**Splitter / Merger** — Structural agents for atomicity enforcement (Splitter divides notes carrying multiple ideas; Merger unifies notes covering the same idea). → [01-agents-and-council.md](01-agents-and-council.md)

**Steelman** — Agent with two roles: constructive (strengthen weak notes) and gate (mandatory rationality check on all critique). → [01-agents-and-council.md](01-agents-and-council.md), [ADR 0007](adrs/0007-steelman-rationality-gate.md)

**Synthesizer** — Thinking agent that proposes new evergreen notes from clusters of related material. → [01-agents-and-council.md](01-agents-and-council.md)

## T

**Tag namespace** — A reserved prefix in the tag system. User-owned: `topic/*`, `area/*`, `type/*`, `status/*`. Agent-only-write: `engram/*`. → [06-note-conventions.md](06-note-conventions.md)

**Tiered model escalation** — Agents start at the cheapest model tier (`fast`) and escalate to `standard` or `deep` only on schema-invalid output, low confidence, or explicit self-request. ~10× per-call cost reduction with quality preserved. → [ADR 0011](adrs/0011-tiered-model-escalation.md)

**Tool-use over generation** — System-wide design principle: prefer deterministic tool calls (sqlite, graph, regex, embedding lookup) over LLM generation for any subtask whose answer is deterministic. The LLM is reserved for genuine fuzzy judgment. Cheaper, faster, more inspectable. → [ADR 0013](adrs/0013-tool-use-over-generation.md)

**Trust ceremony** — A deliberate flow when an agent's trust level changes, combining Auditor's qualitative reading and Watcher's quantitative metrics. → [01-agents-and-council.md](01-agents-and-council.md)

**Trust score** — A per-agent classification (low / medium / high) maintained by Watcher based on outcome metrics. Modulates confidence thresholds. → [01-agents-and-council.md](01-agents-and-council.md)

**Tutor** — Pedagogical agent that generates spaced-repetition flashcards (FSRS) from evergreen notes. → [01-agents-and-council.md](01-agents-and-council.md)

## U

**ULID** — Universally Unique Lexicographically Sortable Identifier. Engram's choice for stable note IDs. Time-prefixed, sortable, globally unique. Lives in frontmatter `id:`. → [06-note-conventions.md](06-note-conventions.md), [ADR 0006](adrs/0006-pure-title-slug-filenames.md)

**Untangler** — On-demand agent that produces sensemaking maps when the user is stuck on a confusing topic. → [01-agents-and-council.md](01-agents-and-council.md)

## V

**Vault** — The root directory of markdown notes (typically an Obsidian vault) that engram operates on. The canonical store. → [00-overview.md](00-overview.md)

**Voice Keeper** — Personal agent that learns the user's writing voice and protects against agent-driven homogenization. → [01-agents-and-council.md](01-agents-and-council.md)

## W

**Watcher** — Meta-agent that continuously monitors agent metrics (acceptance, survival, cost, trust). Distinct from Auditor (quarterly qualitative). → [01-agents-and-council.md](01-agents-and-council.md)

**Witness** — Personal agent that acknowledges journal/personal notes. On-device only, no memory, no cross-agent integration. Local-only LLM. → [01-agents-and-council.md](01-agents-and-council.md)

**Write intents** — Sqlite table used to make markdown+sidecar+sqlite triple-writes atomic. An intent row is created before any file is written; on startup, uncommitted intents are reconciled (replayed or rolled back). → [03-architecture.md](03-architecture.md)
