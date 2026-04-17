# Roadmap

## Purpose

The design corpus (docs 00--11) describes the system engram aspires to be: ~35 agents, council deliberation, corpus digestion, external personal-context API, ceremonies and flows. **That is not v1.** This doc picks the v1 scope and phases the rest. Without this decision, implementation sprawls and nothing ships.

## Scoping principles

1. **Each phase produces a usable system, not a half-built one.** A user could stop at v1 and have a valuable tool. Same for v1.1, v1.2, etc. No phase ends with "doesn't quite work without the next phase."
2. **Ship the foundation before the smart stuff.** Indices, sidecars, git-safety, and Swift capture are foundational. Thinking agents sit on top. Inverting this order means rebuilding under load.
3. **Defer everything speculative.** Dream mode, agent spawning, cloud relay, multi-user --- not v1, not v2. They're plausible v3+ ideas with no current evidence of demand.
4. **Personal use first; product surface later.** External MCP is genuinely valuable but the user needs to use engram for themselves before exposing it to other apps. v2 problem.
5. **Each phase has explicit acceptance criteria.** Not "feels good" --- specific behaviors that demonstrate the phase is done.

## v1 --- Foundation (target: ~5--6 months)

> **Timeline note.** This was originally scoped at ~3 months. A scope-feasibility review (the four-agent design audit) found that realistic effort for the v1 set is closer to 5--6 months for one developer with Claude Code assistance. Rather than cut v1 scope to fit 3 months, we accepted the longer timeline. Subsequent phase targets below are relative to v1 completion, so they shift in absolute calendar terms but not in relative ordering.

### Scope

**Core infrastructure:**
- Vault + git + `.engram/` layout per `06-note-conventions.md`
- SQLite index: `notes`, `links`, `tags`, `artifacts`, `agent_actions`
- Embeddings: local `bge-m3` via ONNX, or cloud OpenAI as opt-in
- Hybrid retrieval: BM25 + dense + RRF + graph expansion (rerank deferred to v1.1)
- File watcher with debounced reindexing
- `gix`-backed git operations, **read-only and unstaged-write only** (no commits ever from the agent host)
- macOS Keychain integration for provider API keys
- Backup Watcher (monitors backup recency; doesn't perform backup)
- Schema migrations (numbered, forward-only)

**Five v1 agents:**
- **Linker** --- propose wikilinks (auto-land at high confidence, propose otherwise)
- **Gardener** --- dead link removal, TODO cleanup, basic staleness flags
- **Cartographer** --- maintain `index.md` and basic MOCs (no quarterly tag audit yet)
- **Scribe** --- fleeting-note cleanup
- **Ingestor** --- file-drop → literature note (text + PDF + image via Claude vision; audio deferred to v1.2)

**Confidence-gated autonomy:**
- Per-agent `auto_land_min_confidence` (default 0.85)
- All writes go to working tree, unstaged
- `agent_actions` table logs every action with confidence and rationale
- Calibration data collected but not yet used to tune

**Swift app (capture-first):**
- iOS + macOS universal binary
- Capture: text, voice (cloud Whisper API), share-sheet, document picker, drag-drop
- Offline capture queue (SwiftData, idempotent via ULID)
- Diff review (per-file diff with agent attribution; tap-to-stage, swipe-to-discard)
- Basic search (calls `/search` API; no offline FTS index yet)
- Cost dashboard
- Connection switcher (single Mac for v1; multi-instance in v2)

**Internal MCP server:**
- stdio transport for Claude Desktop
- Tools: `search_notes`, `grep_notes`, `read_note`, `list_tags`, `follow_backlinks`, `recent_changes`, `read_index`
- No external MCP yet

**REST API (axum):**
- `/notes`, `/notes/:id`, `/search`, `/graph/:id`
- `/ingest` (multipart upload)
- `/changes`, `/changes/:path/stage`, `/changes/:path/discard`, `/commit`
- `/agents`, `/agents/:name/run`
- `/status`, `/events` (SSE)

**CLI:**
- `engram serve`
- `engram reindex [--full]`
- `engram ingest <file>`
- `engram run <agent>`
- `engram status`
- `engram migrate`
- `engram secrets <set|rotate|list>`

### Out of v1 (deferred)

- Council deliberation
- All thinking agents (Synthesizer, Devil's Advocate, Heretic, Inquirer, etc.)
- Personal agents (Biographer, Voice Keeper, Witness)
- Temporal agents (Predictor, Annual Review)
- Curator (corpus digestion)
- External MCP
- Auditor + outcome metrics + prompt evolution
- Pacekeeper
- Trust scores (everything runs at "medium" trust)
- Audio extraction (Whisper local; cloud Whisper API is fine for v1)
- Cross-encoder reranker
- Apple Watch + Shortcuts integrations

### Acceptance criteria

A user can:
- Capture text/voice/files on iOS, see them sync to the Mac, and review them in Obsidian within seconds.
- Drop a PDF and have a literature note land for review.
- Watch agents propose wikilinks, fix dead links, and update the index --- with every change visible via `git diff` and stage-able from the Swift app.
- Review every agent action, with confidence and rationale shown.
- Run for a month without a token-cost surprise (system-wide cost ceiling enforced).
- Lose the index entirely and rebuild from vault + git.
- Use Claude Desktop with the vault exposed via internal MCP.

The acceptance test: **the user prefers using engram over plain Obsidian for daily note-taking.** If that's true, v1 has earned the right to ship more.

---

## v1.1 --- Critical thinking (target: ~6 weeks after v1)

### Scope

**Council deliberation engine:**
- State machine: `DRAFT -> CRITIQUE -> REVISE -> CONVERGE -> {LAND | PROPOSE | SHELVE}`
- Deliberation transcripts in `.engram/deliberations/`
- Quorum selection
- The **Steelman rationality gate** for critical agents

**Thinking agents:**
- **Synthesizer** --- propose new evergreen notes from clusters
- **Devil's Advocate** --- critique (gated by Steelman)
- **Steelman** --- both constructive role and gate role
- **Inquirer** --- 4-mode question generation (replaces 4 prior agents)
- **Heretic** --- sustained counter-arguments (gated)
- **Confidence Annotator** --- demand explicit confidence markers
- **Source Demand** --- demand citations

**Pair-Thinking:**
- Live writing collaborator
- Conversational state machine
- Swift app side-panel UI
- Bounded rounds (3--5 per session)

**Swift app additions:**
- Today widget (pending diffs, due flashcards, conversation prep)
- "I'm stuck on..." and "What do I think about..." quick-launch (Untangler + Research Council)
- Conversation surface for conversational agents
- Apple Shortcuts integration

**Cross-encoder reranker** added to retrieval pipeline (BM25 + dense + RRF + **rerank** + graph).

### Acceptance criteria

- User can ask "what do I think about X?" and get a structured briefing.
- Critical agents only produce rational, steelmanned critique --- the user no longer sees throwaway contrarianism.
- Pair-Thinking changes the writing experience (subjective but observable).

---

## v1.2 --- Personal context (target: ~2 months after v1.1)

### Scope

**Personal agents:**
- **Biographer** --- the user model (`meta/biography.md`)
- **Voice Keeper** --- protect authorial voice; participate in council
- **Witness** --- private acknowledgment for journal notes (local-only)

**Temporal:**
- **Predictor** --- ledger + calibration profile
- **Annual Review** --- yearly long-form reflection

**Pedagogical:**
- **Tutor** --- spaced-repetition flashcards (FSRS); Swift app review session

**Audio extraction:**
- Local `whisper.cpp` for voice memos and long-form audio
- Apple Watch capture app
- Long-form audio session UI in Swift app

### Acceptance criteria

- The system maintains a coherent model of the user; other agents read it and ground their work in it.
- Voice Keeper catches and proposes fixes to homogenized prose.
- Predictor's calibration profile starts to be useful after ~3 months of resolved predictions.

---

## v1.3 --- Corpus digestion (target: ~2 months after v1.2)

### Scope

**Curator** + corpus digestion pipeline per `05-corpus-digestion.md`:
- Survey → plan review → batch digestion → review → integration → audit
- Six dispositions
- Cluster-level synthesis
- Resumable across days/weeks
- `notes/archive/` and `type: archive` for verbatim preservation

**Inbox Triage** for fleeting note classification.

**Structural agents:**
- **Splitter**
- **Merger**
- **Bridge Builder**

### Acceptance criteria

- The user can point engram at `notes-2022-03/` and end with a curated engram vault.
- Compression ratio ≥ 5x (source corpus ≥ 5x larger than resulting active engram content).
- The user trusts the discard decisions (Auditor sample of discards shows < 5% regret).

---

## v2 --- External context layer (target: ~3 months after v1.3)

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
