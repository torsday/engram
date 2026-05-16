# Roadmap

## Purpose

The design corpus (docs 00--11) describes the system engram aspires to be: ~35 agents, council deliberation, corpus digestion, external personal-context API, ceremonies and flows. **That is not v1.** This doc picks the v1 scope and phases the rest. Without this decision, implementation sprawls and nothing ships.

## Scoping principles

1. **Each phase produces a usable system, not a half-built one.** A user could stop at v1 and have a valuable tool. Same for v1.1, v1.2, etc. No phase ends with "doesn't quite work without the next phase."
2. **Ship the foundation before the smart stuff.** Indices, sidecars, git-safety, and Swift capture are foundational. Thinking agents sit on top. Inverting this order means rebuilding under load.
3. **Defer everything speculative.** Dream mode, agent spawning, cloud relay, multi-user --- not v1, not v2. They're plausible v3+ ideas with no current evidence of demand.
4. **Personal use first; product surface later.** External MCP is genuinely valuable but the user needs to use engram for themselves before exposing it to other apps. v2 problem.
5. **Each phase has explicit acceptance criteria.** Not "feels good" --- specific behaviors that demonstrate the phase is done.

## v1 --- Full personal-use engram (target: ~14 months)

> **Timeline + scope evolution.** This was originally scoped at ~3 months as a thin foundation phase, then revised to 5--6 months after the feasibility audit, then expanded again when the user asked for v1 to be feature-complete for engram's intended shape. The current scope absorbs what were previously planned as v1.1 (critical thinking), v1.2 (personal context), and v1.3 (corpus digestion). v2+ remains separately phased (external-facing surfaces and self-improving meta-agents). Timeline for the expanded v1: ~14 months for one developer with Claude Code assistance.

### Scope

v1 is the **full personal-use engram**: foundation + critical thinking + personal context + corpus digestion. Everything required for a single user to depend on the system for daily knowledge work. External-facing surfaces (the MCP scope-and-consent system that lets the user's other apps query engram) and self-improving meta-agents (Auditor, prompt evolution, Pacekeeper) remain in v2+.

**Core infrastructure:**
- Vault + git + `.engram/` layout per `06-note-conventions.md`
- SQLite index (`notes`, `links`, `tags`, `artifacts`, `agent_actions`, plus all the tables defined in `03-architecture.md`)
- LanceDB for vector storage at `.engram/vectors/` (per [ADR 0014](adrs/0014-lancedb-vector-storage.md))
- Embeddings: local `bge-m3` via ONNX, or cloud OpenAI as opt-in; cached by content hash (per [ADR 0012](adrs/0012-embedding-cache-by-content-hash.md))
- Hybrid retrieval: BM25 (FTS5) + dense ANN (LanceDB HNSW with metadata filtering) + RRF + cross-encoder rerank + graph expansion
- File watcher with debounced reindexing + LanceDB reconciliation
- `gix`-backed git operations, **read-only and unstaged-write only** (per [ADR 0003](adrs/0003-no-agent-commits.md) and [ADR 0009](adrs/0009-git-read-write-boundary.md))
- macOS Keychain integration for provider API keys
- Backup Watcher (monitors backup recency)
- Schema migrations (numbered, forward-only) including LanceDB dataset versioning
- System-wide cost ceiling + per-agent token budgets
- Atomic markdown+sidecar+SQLite triple-writes; LanceDB downstream eventually consistent
- Prompt caching (static head + dynamic tail, per [ADR 0010](adrs/0010-prompt-caching-first-class.md))
- Tiered model escalation (start cheap, escalate on need, per [ADR 0011](adrs/0011-tiered-model-escalation.md))
- Tool-use over generation as a design discipline (per [ADR 0013](adrs/0013-tool-use-over-generation.md))
- Streaming structured output with early-exit + request coalescing

**Agent roster (all of the following):**

*Maintenance:* Linker, Gardener, Cartographer (continuous mode + quarterly tag audit), Historian

*Processing:* Scribe, Ingestor (text + PDF + image + web URL + audio via local whisper.cpp), Inbox Triage, Curator

*Structural:* Splitter, Merger, Bridge Builder

*Thinking:* Synthesizer, Devil's Advocate, Steelman (constructive + rationality-gate roles), Inquirer (4-mode), Heretic, Confidence Annotator, Source Demand, Pair-Thinking

*Personal:* Biographer, Voice Keeper, Witness (on-device only, local-only LLM)

*Temporal:* Predictor (predictions ledger + calibration profile), Annual Review

*Pedagogical:* Tutor (FSRS-4.5 spaced-repetition flashcards)

*Meta:* Watcher (basic — continuous metric collection + trust scores; full evaluation in v2.1), Completion Nudger, Backup Watcher

**Council deliberation engine:**
- State machine: `DRAFT → CRITIQUE → REVISE → CONVERGE → {LAND | PROPOSE | SHELVE}` per `01-agents-and-council.md`
- Quorum selection
- Deliberation transcripts in `.engram/deliberations/`
- Per-round vote rows in `deliberation_votes`
- **Steelman rationality gate** for critical agents
- Confidence-gated autonomy: per-agent `auto_land_min_confidence`; below threshold → proposal; above → unstaged write
- Proposal-without-council format for v1 (per `12-agent-spec-template.md`) supports cases where individual agents propose outside a full council session

**Coordinated flows:**
- Evergreen birth ceremony
- Daily standup
- Insight harvest (basic — full prompt-evolution loop in v2.1)
- Flow orchestrator state machine + cost-aware planning per `01-agents-and-council.md`

**Eval framework:**
- Per-agent benchmark suite (`.engram/evals/<agent>/cases/`)
- Quarterly baseline runs
- CI gate on prompt changes
- Bootstrap with 5-10 seed cases per v1 agent

**Corpus digestion (Curator):**
- Survey → plan review → batch digestion → review → integration → audit per `05-corpus-digestion.md`
- Six dispositions (keep-evergreen-draft, keep-literature, merge-into, archive, discard, defer)
- Cluster-level synthesis
- Resumable across days/weeks
- `notes/archive/` and `type: archive` for verbatim preservation

**Swift app (capture-first universal app):**
- iOS + macOS universal binary
- Capture: text, voice (local whisper.cpp + cloud Whisper API option), share-sheet, document picker, drag-drop, camera, smart paste, capture batches, voice memo from Apple Watch, lock-screen widget, Action Button binding
- Apple Shortcuts integration
- Offline capture queue (SwiftData, idempotent via ULID)
- Diff review: per-file diff with agent attribution + confidence + rationale-on-tap; tap-to-stage, swipe-to-discard, long-press-to-amend, discard-with-reason
- Bulk actions + keyboard shortcuts (macOS)
- Snooze
- Search (calls `/search` API; offline fall-back to local SwiftData FTS index)
- Today widget + Lock Screen widget
- Conversation surface for conversational agents (token-streaming via SSE)
- Flashcard review session
- Predictions-due widget
- Cost dashboard
- Backup status indicator
- Connection switcher (single-instance for v1; multi-instance and external-MCP client manager in v2)
- Annual Review viewer (typography-tuned full-screen render)
- Witness inbox (on-device only)
- Spotlight integration (CoreSpotlight)
- Handoff
- Universal Clipboard awareness

**Internal MCP server:**
- stdio transport for Claude Desktop / Code
- Tools: `search_notes`, `grep_notes`, `read_note`, `list_tags`, `follow_backlinks`, `follow_links`, `recent_changes`, `read_index`, `read_biography`, `trace_concept`, `list_predictions`, `due_flashcards`, `list_contradictions`, `vault_health`

**REST + SSE API:**
- All endpoints from `03-architecture.md` §API surface except the external-MCP-management endpoints (which ship in v2)

**CLI:**
- `engram serve`, `engram reindex [--full]`, `engram ingest <file>`, `engram run <agent>`
- `engram digest <path>`, `engram trace <concept>`, `engram untangle <topic>`, `engram prep`, `engram standup`
- `engram council <question>`, `engram proposals [list|approve|reject]`
- `engram flow [resume|retry|estimate]`
- `engram eval <agent>`
- `engram backup verify`
- `engram migrate`, `engram secrets <set|rotate|list>`, `engram status`

**First-run / onboarding** per `08-first-run.md`: wizard, bootstrap mode (first 30 days at 0.95 threshold), sparse-content handling for context agents, default agent set, tutorial vault.

### Out of v1 (deferred to v2 and beyond)

- **External MCP server** — the scope/consent/audit-based personal-context API that lets the user's other apps connect (v2; per `04-external-mcp.md`)
- **Auditor** — quarterly qualitative deep evaluation (v2.1)
- **Outcome-based metrics** beyond accept/reject (v2.1)
- **Prompt evolution** (shadow-mode A/B variants with Auditor-proposed swaps) (v2.1)
- **Trust scores modulating confidence thresholds** (v2.1)
- **Pacekeeper** throttling (v2.1; v1 uses fixed thresholds)
- **Analogist, Assumption Excavator, Socratic Prober, Contradiction Detector** — these thinking agents land in v2.2
- **Untangler, Research Council, Conversation Prep, Debate Mode** — on-demand orchestrators land in v2.2
- **Scout, Fact Checker** — external-source agents land in v2.2
- **Speculative features** (Dream mode, agent spawning, goal-directed sessions, cloud relay, multi-user) — v3+ if at all

### Acceptance criteria

Per `/SPEC.md` for the machine-readable checklist. Summarized:

A user can:
- Capture text/voice/files/photos on iOS or macOS, see them sync to the Mac, and review them within seconds.
- Drop a PDF, image, web URL, or audio file and have a literature note land for review.
- Point engram at an existing Obsidian vault (~9K notes) and end with a curated engram vault at ≥5× compression; the user trusts the discard decisions.
- Watch agents propose wikilinks, fix dead links, update the index, propose new evergreen notes, write heretical counter-notes, demand citations, surface contradictions — with every change visible via `git diff` and stage-able from the Swift app.
- Ask a question and get a council briefing or trace concept evolution over time.
- Engage Pair-Thinking during live writing.
- Have a working spaced-repetition flashcard practice from any evergreen note.
- Receive a year-end Annual Review.
- Review every agent action with confidence and rationale shown.
- Run for a month without a token-cost surprise.
- Lose the SQLite index entirely and rebuild from vault + git.
- Use Claude Desktop with the vault exposed via internal MCP.

The "earned the right to ship more" test (per `/SPEC.md`) holds.

---

## v2 --- External context layer (target: ~3 months after v1)

### Scope

**External MCP server** per `04-external-mcp.md`:
- HTTP+SSE transport
- API key + scope-based auth
- Consent flow via Swift app
- Privacy-zone default-deny
- Audit log

**Personal-context tools:**
- `personal_context`, `preferences`, `recent_thinking_on`, `ask_user`, `record_session`

**MCP client manager** in Swift app:
- Per-client audit views, scope management, revoke, "what does this app see?" preview

**Travel-app reference implementation** as a demonstration client.

### Acceptance criteria

- A separate app (the reference travel app) successfully grounds its planning in engram's personal context.
- Audit log shows what each client accessed; revoke works.
- Privacy zones never leak.

---

## v2.1 --- Self-improving swarm

### Scope

**Auditor** (deep qualitative evaluator) + Auditor outputs in `.engram/meta/audits/`.

**Outcome metrics** beyond accept/reject:
- Survival (30/90/180-day)
- Engagement (visited/linked/modified after)
- Downstream productivity
- Reversal

**Trust scores** modulating per-agent confidence thresholds.

**Pacekeeper** — system-wide throttle when user is overwhelmed.

**Prompt evolution** (shadow-mode A/B variants, Auditor proposes swaps).

### Acceptance criteria

- Per-agent trust scores reflect real outcomes, not just acceptance.
- Pacekeeper visibly throttles the swarm when backlog grows; relaxes when caught up.
- At least one prompt variant has been promoted via the evolution loop.

---

## v2.2 --- The rest of the thinking layer

### Scope

The remaining thinking and on-demand agents:
- **Analogist** (cross-domain parallels)
- **Assumption Excavator**
- **Socratic Prober** (with evergreen birth ceremony)
- **Contradiction Detector**
- **Untangler**
- **Research Council** + Debate Mode
- **Conversation Prep** (with calendar integration)
- **Scout** (RSS / external feeds)
- **Fact Checker**

**Coordinated flows:**
- Evergreen birth ceremony
- Daily standup
- Insight harvest
- Trust ceremony

### Acceptance criteria

- The full thinking layer is operational. The user has flows for every major intellectual move (synthesize, challenge, untangle, prepare, audit).

---

## v3+ --- Speculative

Anything else from the design corpus that hasn't shipped by v2.2. Reconsidered based on actual usage data and learnings. Candidates (no commitment):

- **Dream mode** --- speculative idle-time exploration
- **Agent spawning** --- agents proposing new agents
- **Goal-directed sessions** --- temporary agent constellation around a research goal
- **Cloud relay** for external MCP
- **Multi-user / multi-vault** sharing
- **Web review UI** (probably never; Swift handles it)
- **Plugin / extension system** for third-party agents

## Anti-scope --- never in any version

- **Free-form chat with the vault.** Engram is explicitly not a chatbot. Research Council is structured; Pair-Thinking is bounded; Untangler produces a map. None of these are open-ended chat. This stays.
- **Hosted / SaaS engram.** Personal tool. Self-hosted only. v3 cloud relay is for personal access from anywhere, not multi-tenant hosting.
- **End-to-end encryption of the vault.** Out of scope. The vault is plaintext markdown; users can use FileVault or whatever local encryption they want.
- **Real-time collaboration.** Engram is single-user. Two people sharing a vault is git-merge territory, not collaboration features.
- **Automatic publishing.** Publisher (mentioned in early brainstorming) is not in any phase. Engram's value is private thinking, not public output. Publishing is a separate app that can build on the external MCP if desired.

## The shipping principle

Each phase ends with **a thing the user is happy to keep using indefinitely if no further work happens.** Engram should never feel half-built. The roadmap is a sequence of complete experiences, not partial steps toward a hypothetical end-state.

If v1 ships and the user prefers it over plain Obsidian: engram has earned the right to exist. Every phase after that earns the right to exist by demonstrating the same.
