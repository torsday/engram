# Architecture

## Guiding constraints

1. **Single binary.** `engram` is one Rust binary that serves the API, runs agents, watches files, hosts the MCP server, and manages git. No sidecars, no Docker, no microservices. One process, one install.
2. **Vault is canonical; indices are caches.** SQLite, embeddings, and the link graph are derived from the vault. `engram reindex` rebuilds everything from markdown + git. This forces clean separation between storage and computation.
3. **Agents are data.** Prompt files + TOML config, hot-reloaded at runtime. The Rust binary never needs to be recompiled to add, modify, or remove an agent.
4. **One API, many surfaces.** Swift app, web UI, CLI, and MCP all consume the same HTTP+SSE interface (with MCP as a separate transport).
5. **Local-first, cloud-optional.** Embeddings, extraction, and generation can all run locally. Cloud providers are opt-in for quality or speed.

---

## Tech stack

| Layer               | Choice                    | Crate / tool                    | Rationale                                                                                                                                                                                                                        |
| ------------------- | ------------------------- | ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Language            | Rust                      | ---                             | Single binary, excellent SQLite/filesystem ergonomics, type system pays for deliberation state machines and tool schemas. No runtime install for users.                                                                          |
| Async runtime       | Tokio                     | `tokio`                         | Standard. Needed for concurrent agent runs, file watching, HTTP serving.                                                                                                                                                         |
| HTTP server         | Axum                      | `axum`                          | Tokio-native, tower middleware, SSE support built-in.                                                                                                                                                                            |
| Markdown parser     | Comrak                    | `comrak`                        | CommonMark + GFM, AST access for structural edits. Custom extensions for wikilinks and frontmatter.                                                                                                                              |
| Frontmatter         | serde_yaml                | `serde_yaml`                    | YAML frontmatter is the Obsidian standard.                                                                                                                                                                                       |
| SQLite              | rusqlite                  | `rusqlite`                      | Direct bindings. One file for metadata, FTS, and vectors.                                                                                                                                                                        |
| Full-text search    | SQLite FTS5               | (built into rusqlite)           | BM25 scoring, incremental updates. Catches exact-term matches that embeddings miss.                                                                                                                                              |
| Vector search       | LanceDB (embedded)        | `lance`, `lancedb`              | Rust-native embedded columnar vector DB. Scales to 100K+ notes without re-architecture. Native HNSW indexing; built-in dataset versioning. Stored under `.engram/vectors/`. See [ADR 0014](adrs/0014-lancedb-vector-storage.md). |
| Embeddings (local)  | ONNX Runtime              | `ort`                           | Run `bge-m3` or `nomic-embed-text-v2` in-process on Apple Silicon. No Ollama dependency for embeddings.                                                                                                                          |
| Embeddings (cloud)  | OpenAI                    | `async-openai`                  | `text-embedding-3-large`, Matryoshka-reducible. ~$0.50 for 10K notes.                                                                                                                                                            |
| LLM providers       | Anthropic, OpenAI, Ollama | `reqwest` + typed wrappers      | Provider trait with implementations. Agents specify a model tier (`fast`/`standard`/`deep`), the system maps to a concrete model.                                                                                                |
| Git                 | Gitoxide                  | `gix`                           | Pure Rust, no libgit2. Commit-as-agent, branch management, diff.                                                                                                                                                                 |
| File watching       | notify                    | `notify`                        | Debounced, cross-platform. Triggers indexing and agent runs.                                                                                                                                                                     |
| Audio transcription | whisper.cpp               | `whisper-rs`                    | Local, fast on Apple Silicon.                                                                                                                                                                                                    |
| OCR                 | ocrs                      | `ocrs`                          | Rust-native OCR. Fallback when cloud vision is unavailable or privacy-gated.                                                                                                                                                     |
| MCP server          | Rust MCP SDK              | `rmcp`                          | Exposes vault tools to Claude Desktop/Code. Same process, same index access.                                                                                                                                                     |
| CLI                 | clap                      | `clap`                          | Standard. Subcommands: `serve`, `reindex`, `run`, `status`, `ingest`.                                                                                                                                                            |
| Config              | TOML                      | `toml`                          | Agent configs, vault config, privacy zones.                                                                                                                                                                                      |
| Logging / tracing   | tracing                   | `tracing`, `tracing-subscriber` | Structured, async-aware. JSON output for machine consumption, human-readable for CLI.                                                                                                                                            |
| Serialization       | serde                     | `serde`, `serde_json`           | Everywhere.                                                                                                                                                                                                                      |

### Swift app

The Swift app has four primary roles, in priority order: **capture, diff review, search, browse.** Capture is the highest-priority surface --- the failure mode of a knowledge tool is friction at the entry point.

| Layer         | Choice                                                                                 | Rationale                                                                                                           |
| ------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Framework     | SwiftUI                                                                                | Universal (iOS + macOS). Declarative, native feel on both platforms.                                                |
| Concurrency   | Swift concurrency (async/await)                                                        | Native, structured. No Combine for new code.                                                                        |
| Networking    | URLSession + async                                                                     | Talks to `engram serve` over HTTP. SSE for real-time agent events.                                                  |
| Local storage | SwiftData                                                                              | Two roles: (1) metadata cache for offline browse/search; (2) **offline capture queue** (see below).                 |
| Capture       | Share extension + document picker + drag-drop + voice memo                             | iOS: share sheet + document picker + voice memo. macOS: drag-drop window + share extension. Bound to global hotkey. |
| Diff review   | Per-file diff with agent attribution, confidence score, rationale-on-tap               | Tap to stage (`git add`); swipe-to-discard (`git restore`); long-press to amend before staging.                     |
| Search        | Hybrid retrieval against `/search` API; offline fall-back to local SwiftData FTS index | Useful when away from Obsidian or when reviewing context for a capture.                                             |

#### Offline capture queue (iOS)

The Swift app's capture surface must work even when the Mac-hosted `engram serve` is unreachable (Mac asleep, no network, traveling). Captures land in a local SwiftData store and sync when the server is reachable again.

```
Capture (any source: voice / text / share / file / photo)
    │
    ▼
Local SwiftData queue
  ┌──────────────────────────────────────────────────┐
  │  id: ULID (assigned client-side, sortable)       │
  │  payload: text | audio | image | file ref        │
  │  metadata: source, captured_at, geo (opt)        │
  │  status: pending | syncing | acked | failed      │
  │  content_hash: SHA-256 (computed locally)        │
  │  attempts: integer (for retry/backoff)           │
  └──────────────────────────────────────────────────┘
    │
    ▼ when reachable: POST /ingest with idempotency_key = ULID
engram core (Mac)
    │
    ▼ acks back; client marks `acked` and drops from queue
```

Properties:

- **Idempotent.** Server uses the ULID as an idempotency key. Re-sending a previously-acked capture is a no-op. Resilient to retries, app restarts, and unreliable mobile networks.
- **Content-addressed.** SHA-256 hash matches what the server's artifact store computes, so dedup works across the queue/server boundary.
- **Lossless.** Captures persist across app restarts. Failed captures stay in the queue for inspection rather than silently dropping.
- **Bounded.** Configurable queue size (default 500 items). When full, oldest are exported to the iOS Files app for manual recovery rather than discarded.
- **Visible.** A small badge in the Swift app shows queue depth and sync status. Users can see what's pending and force a manual retry.

#### Diff review interface

The Swift app's primary review surface is `git diff` against the working tree, presented per-file:

```
┌─────────────────────────────────────────┐
│  Pending agent changes (3 files)        │
│  ─────────────────────────────────────  │
│  notes/evergreen/attention.md           │
│  +3 lines, -1 line  ·  linker (0.93)    │
│  ┌───────────────────────────────────┐  │
│  │ + [[Compression]]                 │  │
│  │ - lossy compression               │  │
│  │ + lossy compression of context    │  │
│  └───────────────────────────────────┘  │
│  [Stage]  [Discard]  [Amend]  [Why?]    │
└─────────────────────────────────────────┘
```

`Why?` opens the agent's rationale, retrieval signals, and confidence breakdown from the `agent_actions` row. Stage runs `git add <path>`. Discard runs `git restore <path>`. Amend opens a quick text editor against the diff before staging.

#### Capture surfaces (zero-friction entry points)

Capture is the highest-priority surface; the failure mode of a knowledge tool is friction at the entry point. In addition to the standard share-extension and document-picker, the Swift app exposes:

- **Apple Shortcuts integration.** Engram actions (`capture`, `ask`, `prep`, `untangle`) are exposed as Shortcuts. Enables: "Hey Siri, capture to engram"; Action Button binding (iPhone 15+); automation triggers ("when I leave a place, capture a reflection"). Force multiplier --- every Shortcut a user builds becomes an engram entry point essentially for free.
- **Apple Watch capture app.** Voice/dictation capture only (no review, no search). Captures land in the watch's local SwiftData queue, sync via the iPhone when reachable. Targets the walking-and-thinking use case the Watch is uniquely good at.
- **Lock-screen widget + Action Button binding.** One-tap voice memo from the lock screen. Widget shows current queue depth and sync status.
- **Camera capture.** Photo of whiteboard, book page, slide, business card, screenshot → routed through the Ingestor pipeline with vision-model extraction (Claude vision preferred, local OCR fallback). Produces a literature-note draft.
- **Smart paste.** Single paste handler routes content by type: URL → fetch + extract → literature note candidate; image → artifact + vision extraction; text → fleeting note (with source-app attribution from clipboard metadata when available).
- **Capture batches.** Group related captures into a session (a meeting, a walk, a reading session). The Ingestor processes the batch as a unit; Linker treats batched captures as a coherent semantic neighborhood; connections within the batch get extra weight.
- **Long-form audio with local Whisper.** Captures longer than ~1 minute go through chunked local `whisper.cpp` transcription. Walking-and-thinking is one of the most productive note modes; native long-form support without cloud dependency makes this real.
- **Privacy flag on capture.** A toggle visible before submit. When set, the capture is tagged `engram/private`, processed by local-only models, and excluded from external MCP regardless of zone configuration.

#### Diff-review power features

For users who spend hours in the diff queue, especially on macOS:

- **Bulk actions + keyboard shortcuts (macOS).** Navigate entirely from the keyboard: J/K to move between files, S to stage, D to discard, A to amend, ⌘-A to select all-from-this-agent, ⌘-Shift-S to stage-all-selected. Mouse use is optional.
- **Discard with reason.** When rejecting a change, optionally tag the reason: hallucinated, wrong direction, redundant, out of scope, voice-mismatch. Tagged reasons feed the `agent_actions` row's `human_decision` and become input to Auditor's quarterly evaluations + the prompt-evolution loop. **This is how the swarm learns from the user beyond accept/reject signal.**
- **Snooze.** Defer a change for N days without staging or discarding. The `agent_actions` row stays `pending`; the change drops out of the active queue and resurfaces on the chosen date.

#### Agent productivity loops

Surfaces that turn engram's agent capabilities into daily-useful interactions:

- **Today widget + Lock Screen widget.** Glance-worthy: pending diff count, predictions due today, flashcards due today, today's Inquirer question, current Pacekeeper status. The "feels alive without nagging" surface.
- **Conversation Prep cards (calendar integration).** The day before / morning of any calendar event, the Conversation Prep agent generates a briefing for the people and topics involved. Surfaced as a notification card; tap to read; mark "I read it" to acknowledge.
- **Untangler / Research Council quick-launch.** Two prominent buttons in the app's primary surface: **"I'm stuck on..."** (Untangler) and **"What do I think about..."** (Research Council). Voice or text input. The thinking-aid surfaces are engram's most valuable features and should be one tap away.
- **Live conversation surface with token-streaming.** Bounded back-and-forth UI for the conversational agents (Pair-Thinking, Socratic Prober, Research Council follow-ups, Inquirer's conversational mode). Chat-style interface scoped to the agent + topic + bounded round count. "3 rounds remaining" visible so it never feels open-ended. **Agent responses stream token-by-token to the Swift app via SSE** — perceived latency drops from ~5s (wait for full response) to ~500ms (first token visible). The conversation API endpoint (`POST /conversations/:id/turn`) returns a stream of token chunks; the Swift app appends to the visible turn as chunks arrive. Same provider streaming infrastructure used for early-exit (see Streaming structured output with early-exit, in the Error handling section); for conversational turns the output is free-form text rather than structured JSON, so the early-exit logic doesn't apply but the streaming display does.
- **Flashcard review session (Tutor).** Anki-style swipe interface for spaced-repetition cards. Streak tracking. 5-minute sessions on the bus.
- **Predictions due widget.** When a prediction comes due, surface it with three buttons: correct / incorrect / superseded. The user resolves in seconds; calibration data accumulates without conscious effort.

#### System operations

- **MCP client manager.** List of registered external MCP clients (per `04-external-mcp.md`): name, granted scopes, last-active timestamp, per-client access counts, revoke button. Plus a **"what does this app see?"** preview that simulates a `personal_context` call against the client's scopes so the user can audit the data exposure before granting a scope.
- **Cost dashboard.** Current month's token spend. Per-agent breakdown. Sparkline of recent days. Projection vs. configured cost ceiling (see `agent_budgets` schema). Quiet but visible --- users should never be surprised by their bill.
- **Backup status indicator.** "Vault backed up <duration> ago to <target>" --- green check or warning. Pairs with the Backup Watcher gap-fill noted in the prior audit.
- **Connection switcher.** For users with multiple engram instances (home Mac, work Mac, future cloud-hosted) --- pick which instance the current session connects to. Includes "scan local network" and saved Tailscale endpoints.

#### Personal / affective surfaces

- **Witness inbox.** Private acknowledgments from the Witness agent. **On-device only** --- never synced to the Mac, never logged to `agent_actions`, never sent to any cloud LLM. Strong privacy guarantees in the UI (no share buttons, no export, no screenshot watermarking).
- **Annual Review viewer.** When the Annual Review note lands, render it full-screen with typography-tuned layout and scroll-paced reveal. The most emotionally resonant artifact engram produces; the surface should honor that.

#### Cross-cutting OS integrations

- **Spotlight / iOS Search integration.** Vault search via OS-level search. CoreSpotlight indexer keeps results current. "I remember writing about X" is the most common search query the user has; OS-level integration makes it answerable from anywhere.
- **Handoff.** Start a capture on iPhone, finish on Mac. Or start reviewing a diff on Mac, continue on iPhone. Apple's Handoff framework gives this for relatively little code.
- **Universal Clipboard awareness.** When the user copies on Mac and pastes on iPhone (or vice versa), engram detects the same content via SHA-256 and dedups any resulting capture against the clipboard origin.

---

## Crate workspace

```
engram/
├── Cargo.toml                          # workspace definition
├── crates/
│   ├── engram-core/                    # shared types, vault I/O, frontmatter,
│   │                                   # markdown AST, wikilink parser, note ID,
│   │                                   # sidecar JSON read/write, slug + collision
│   │                                   # detection, evergreen rubric checker
│   │
│   ├── engram-index/                   # sqlite manager, FTS5, LanceDB,
│   │                                   # link graph, tag graph, embedding pipeline,
│   │                                   # hybrid search (BM25 + dense + RRF + rerank)
│   │
│   ├── engram-agents/                  # agent host: scheduler, runner, identity,
│   │                                   # tool gateway, prompt loader (hot-reload),
│   │                                   # council engine (deliberation state machine),
│   │                                   # review queue manager, agent memory store,
│   │                                   # trust score tracker, conversation engine,
│   │                                   # dream mode scheduler, goal-directed sessions,
│   │                                   # agent spawning proposals
│   │
│   ├── engram-rubric/                  # evergreen rubric definitions + checker,
│   │                                   # note-type classifier, provenance validator
│   │
│   ├── engram-git/                     # gix-backed: commit-as-agent, branch mgmt,
│   │                                   # worktree isolation, diff generation,
│   │                                   # provenance metadata in commits
│   │
│   ├── engram-llm/                     # provider trait + impls:
│   │                                   #   AnthropicProvider (Opus/Sonnet/Haiku)
│   │                                   #   OpenAIProvider (GPT-4o, embeddings)
│   │                                   #   OllamaProvider (local)
│   │                                   # Model-tier mapping (fast/standard/deep)
│   │                                   # Structured output parsing, tool-use protocol
│   │
│   ├── engram-extract/                 # file classification, extraction dispatch,
│   │                                   # PDF/image/audio/web extractors,
│   │                                   # whisper-rs, ocrs, vision API integration
│   │
│   ├── engram-api/                     # axum HTTP server + SSE:
│   │                                   #   POST /ingest
│   │                                   #   GET /notes, /notes/:id
│   │                                   #   GET /search (hybrid, provenance-filterable)
│   │                                   #   GET /proposals, POST /proposals/:id/approve
│   │                                   #   GET /events (SSE: agent activity, queue updates)
│   │                                   #   POST /council/query (Research Council)
│   │                                   #   GET/POST /conversations (agent dialogues)
│   │                                   #   GET/POST /sessions (goal-directed)
│   │                                   #   GET /dreams (speculative proposals)
│   │                                   #   GET /agents/trust (trust scores)
│   │                                   #   GET /status
│   │
│   ├── engram-mcp/                     # MCP server exposing vault tools:
│   │                                   #   search_notes (semantic)
│   │                                   #   grep_notes (literal)
│   │                                   #   read_note
│   │                                   #   list_tags
│   │                                   #   follow_backlinks
│   │                                   #   write_note (gated)
│   │                                   #   recent_changes
│   │
│   └── engram-cli/                     # binary entry point, clap subcommands:
│                                       #   engram serve [--port]
│                                       #   engram reindex [--full]
│                                       #   engram run <agent> [--note <id>]
│                                       #   engram ingest <file>
│                                       #   engram digest <path>     (corpus digestion, see 05)
│                                       #   engram trace <concept>   (diachronic feature)
│                                       #   engram untangle <topic>
│                                       #   engram prep --with <name> --topic <t>
│                                       #   engram standup            (today's report)
│                                       #   engram status
│                                       #   engram council <question>
│                                       #   engram proposals [list|approve|reject]
│
├── apps/
│   └── engram-ios/                     # Xcode project, SwiftUI universal app
│       ├── Shared/                     # shared code (iOS + macOS)
│       │   ├── Models/                 # API response types, note model
│       │   ├── Services/              # HTTP client, SSE listener
│       │   └── Views/                 # capture, review queue, browse
│       ├── iOS/                        # iOS-specific (share extension)
│       └── macOS/                      # macOS-specific (drag-drop window)
│
├── agents/                             # agent definitions (data, not code)
│   ├── linker/                         # maintenance: discover wikilinks
│   │   ├── prompt.md
│   │   └── config.toml
│   ├── gardener/                       # maintenance: prune stale content
│   ├── cartographer/                   # maintenance: MOCs, index, navigation, tag audits
│   ├── historian/                      # maintenance: activity logs
│   ├── splitter/                       # structural: enforce atomicity
│   ├── merger/                         # structural: unify duplicate concepts
│   ├── bridge-builder/                 # structural: connect isolated clusters
│   ├── scribe/                         # processing: clean up captures
│   ├── ingestor/                       # processing: file -> literature note
│   ├── inbox-triage/                   # processing: classify fleeting notes
│   ├── curator/                        # processing: digest external corpora (see 05)
│   ├── synthesizer/                    # thinking: propose new evergreen notes
│   ├── devils-advocate/                # thinking: critique claims (gated)
│   ├── steelman/                       # thinking: strengthen + serve as rationality gate
│   ├── inquirer/                       # thinking: 4-mode question generator
│   ├── contradiction-detector/         # thinking: find internal conflicts
│   ├── socratic-prober/                # thinking: stress-test before evergreen
│   ├── analogist/                      # thinking: cross-domain parallels
│   ├── assumption-excavator/           # thinking: surface unstated premises
│   ├── confidence-annotator/           # thinking: demand explicit confidence markers
│   ├── source-demand/                  # thinking: demand citations
│   ├── pair-thinking/                  # thinking: live writing collaborator (conversational)
│   ├── heretic/                        # thinking: sustained counter-arguments (gated)
│   ├── biographer/                     # personal: model the user themselves
│   ├── voice-keeper/                   # personal: protect authorial voice
│   ├── witness/                        # personal: acknowledge journal/personal notes
│   ├── predictor/                      # temporal: predictions ledger + calibration profile
│   ├── annual-review/                  # temporal: yearly long-form reflection
│   ├── tutor/                          # pedagogical: spaced-repetition flashcards
│   ├── scout/                          # external: monitor RSS/feeds
│   ├── fact-checker/                   # external: verify claims against web
│   ├── watcher/                        # meta: continuous monitor (numerical, weekly)
│   ├── auditor/                        # meta: deep evaluator (qualitative, quarterly)
│   ├── completion-nudger/              # meta: surface unfinished work
│   ├── pacekeeper/                     # meta: throttle swarm when user is overwhelmed
│   ├── conversation-prep/              # on-demand: meeting/conversation briefings
│   └── untangler/                      # on-demand: sensemaking for confused topics
│
├── docs/
│   └── design/                         # these documents
│
└── tests/
    ├── fixtures/                       # sample vault snapshots for testing
    └── integration/                    # end-to-end: ingest file -> literature note
```

---

## Data flow

### Startup

```
engram serve
    |
    +-- load .engram/config.toml (vault path, providers, privacy zones)
    +-- open/create .engram/index.sqlite (metadata, FTS5, link graph,
    |     agent memory, trust scores, conversations, sessions, dreams,
    |     embedding_cache)
    +-- open/create .engram/vectors/ (LanceDB dataset(s); per ADR 0014)
    +-- run LanceDB <-> SQLite reconciliation pass (catch any missed
    |     async upserts from prior shutdown)
    +-- load agents/ directory (hot-reload watcher on this dir)
    +-- load trust scores from sqlite, apply privilege overrides
    +-- expire stale agent memory entries (TTL cleanup)
    +-- resume active goal-directed sessions
    +-- resume active conversations
    +-- start file watcher on vault path (notify-rs, debounced)
    +-- start axum HTTP server (API + SSE)
    +-- start MCP server (stdio or SSE transport)
    +-- start agent scheduler (tokio tasks per scheduled agent)
    +-- start dream mode scheduler (idle-triggered, lowest priority)
    +-- start Scout feed pollers (per-feed tokio tasks)
    +-- ready
```

### File change event

```
Vault file modified (notify-rs)
    |
    +-- debounce (default 2s)
    +-- parse markdown AST + frontmatter
    +-- update index: metadata row, FTS5, re-embed if content changed, update link graph
    +-- check agent triggers: which agents have trigger = "file_change"?
    +-- for each triggered agent:
    |     +-- load prompt.md (hot-reload check)
    |     +-- assemble context: changed note, relevant neighbors, rubric,
    |     |     biographer model, voice model, sidecar JSON for the note
    |     +-- call LLM with tools; structured output includes confidence
    |     +-- if confidence >= auto_land_min_confidence AND change is at-or-below
    |     |   the agent's max_invasiveness:
    |     |     write change directly to working tree (UNSTAGED) --- both the
    |     |     markdown file AND any sidecar updates (also unstaged)
    |     |     log row in agent_actions with confidence, rationale, diff hash
    |     +-- else:
    |     |     convene council; on LAND, council writes change unstaged
    |     |     and logs to agent_actions with deliberation_id
    |     +-- NEVER `git add` OR `git commit`. Period.
    |
    +-- emit SSE event (for Swift app: "agent change pending", queue depth, etc.)
```

### Ingestion event

```
POST /ingest (file upload)
    |
    +-- hash file, check dedup
    +-- store artifact
    +-- queue Ingestor agent:
    |     classify -> extract -> Scribe drafts literature note
    |     -> Linker proposes connections
    +-- literature note enters review queue (.engram/proposals/)
    +-- emit SSE event ("ingestion complete, 1 proposal ready")
    +-- human approves via Swift app / CLI
    +-- commit literature note to vault
    +-- downstream agents (Synthesizer, Linker) pick up on next pass
```

### Council session

```
Agent proposes change above auto-land threshold
    |
    +-- create deliberation record
    +-- determine quorum (relevant agents)
    +-- DRAFT: proposer submits structured diff + rationale
    +-- CRITIQUE: each participant reviews (parallel LLM calls)
    +-- if request_changes: REVISE -> second CRITIQUE
    +-- CONVERGE: tally votes
    |     +-- LAND: commit to vault
    |     +-- PROPOSE: add to .engram/proposals/
    |     +-- SHELVE: add to .engram/shelved/
    +-- write deliberation transcript to .engram/deliberations/
    +-- emit SSE event
```

---

## Index schema (SQLite)

```sql
-- Note metadata (derived from frontmatter + filesystem)
CREATE TABLE notes (
    id          TEXT PRIMARY KEY,     -- ULID from frontmatter
    path        TEXT NOT NULL UNIQUE, -- relative to vault root
    title       TEXT NOT NULL,
    note_type   TEXT NOT NULL,        -- fleeting, literature, evergreen, moc
    status      TEXT,                 -- draft, candidate-evergreen, evergreen, needs-review
    created_at  TEXT,                 -- ISO 8601 UTC
    modified_at TEXT,                 -- ISO 8601 UTC
    created_by  TEXT,                 -- human or agent name; DERIVED from sidecar
                                      -- provenance for query convenience (e.g.
                                      -- "show all evergreens by Synthesizer").
                                      -- Sidecar is the durable record; this
                                      -- column rebuilds via `engram reindex`.
    frontmatter TEXT,                 -- full YAML as JSON for flexible queries
    content     TEXT                  -- raw markdown body (for FTS)
);

-- Full-text search
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title, content, tags,
    content='notes',
    content_rowid='rowid'
);

-- NOTE: dense vector embeddings live in LanceDB, NOT in SQLite. See ADR 0014.
-- The LanceDB dataset is at `.engram/vectors/notes_v1.lance/` with schema:
--   id (utf8 ULID; join key back to `notes` here),
--   content_hash (utf8; for reconciliation with embedding_cache),
--   embedding (fixed_size_list<float32, N> where N matches active model dim),
--   note_type, created_at, modified_at, tags (denormalized for filtered ANN).
-- The active embedding model is configured in `.engram/config.toml`; multiple
-- model datasets can coexist (switching models doesn't blow away old vectors).
-- The `embedding_cache` SQLite table (below) is the authoritative computed-embedding
-- cache; LanceDB is the queryable ANN index over those vectors.

-- Link graph
CREATE TABLE links (
    source_id   TEXT NOT NULL REFERENCES notes(id),
    target_id   TEXT NOT NULL REFERENCES notes(id),
    context     TEXT,                 -- surrounding sentence for display
    created_by  TEXT,                 -- DERIVED from inline HTML-comment provenance
                                      -- (e.g. <!-- by: linker -->) for query speed.
                                      -- Block-level provenance in the markdown
                                      -- file is the durable record; this column
                                      -- rebuilds via `engram reindex`.
    PRIMARY KEY (source_id, target_id)
);

-- Tag index
CREATE TABLE tags (
    note_id     TEXT NOT NULL REFERENCES notes(id),
    tag         TEXT NOT NULL,
    PRIMARY KEY (note_id, tag)
);
CREATE INDEX idx_tags_tag ON tags(tag);

-- Artifact metadata (for ingestion)
CREATE TABLE artifacts (
    hash        TEXT PRIMARY KEY,     -- SHA-256
    filename    TEXT,
    mime_type   TEXT,
    size_bytes  INTEGER,
    source_url  TEXT,
    dropped_at  TEXT,
    classification TEXT,              -- academic_paper, screenshot, voice_memo, ...
    extraction_status TEXT,           -- received, classified, extracted, drafted, approved
    literature_note_id TEXT REFERENCES notes(id)
);

-- Agent run log (for Watcher)
CREATE TABLE agent_runs (
    id              TEXT PRIMARY KEY,
    agent_name      TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    trigger         TEXT,             -- file_change, cron, on_demand, council
    notes_affected  TEXT,             -- JSON array of note IDs
    outcome         TEXT,             -- auto_land, council_convened, no_action
    deliberation_id TEXT
);

-- Deliberation metadata
CREATE TABLE deliberations (
    id              TEXT PRIMARY KEY,
    convened_by     TEXT NOT NULL,
    participants    TEXT NOT NULL,     -- JSON array
    outcome         TEXT NOT NULL,     -- land, propose, shelve
    created_at      TEXT NOT NULL,
    transcript_path TEXT,             -- relative path to .engram/deliberations/
    session_id      TEXT              -- goal-directed session, if any
);

-- Agent memory (persistent cross-run state)
CREATE TABLE agent_memory (
    agent_name  TEXT NOT NULL,
    key         TEXT NOT NULL,         -- e.g. "rejected:link:noteA:noteB"
    value       TEXT,                  -- JSON payload
    created_at  TEXT NOT NULL,
    expires_at  TEXT,                  -- optional TTL, NULL = permanent
    PRIMARY KEY (agent_name, key)
);
CREATE INDEX idx_agent_memory_expires ON agent_memory(expires_at)
    WHERE expires_at IS NOT NULL;

-- Trust scores (maintained by Watcher)
CREATE TABLE trust_scores (
    agent_name      TEXT PRIMARY KEY,
    total_decisions INTEGER NOT NULL DEFAULT 0,
    accepted        INTEGER NOT NULL DEFAULT 0,
    rejected        INTEGER NOT NULL DEFAULT 0,
    reverted        INTEGER NOT NULL DEFAULT 0,  -- approved then manually undone
    acceptance_rate REAL,                         -- rolling 90-day window
    trust_level     TEXT NOT NULL DEFAULT 'medium', -- high, medium, low
    last_evaluated  TEXT,
    promoted_at     TEXT,                         -- when trust level last changed
    demotion_reason TEXT                          -- if demoted, why
);

-- Conversations (bounded agent-human dialogues)
CREATE TABLE conversations (
    id              TEXT PRIMARY KEY,
    agent_name      TEXT NOT NULL,
    note_id         TEXT REFERENCES notes(id),    -- note being discussed, if any
    status          TEXT NOT NULL,                 -- active, completed, abandoned
    round           INTEGER NOT NULL DEFAULT 0,
    max_rounds      INTEGER NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    transcript      TEXT                          -- JSON array of turns
);

-- Goal-directed sessions
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    goal            TEXT NOT NULL,
    status          TEXT NOT NULL,                 -- active, paused, completed
    created_at      TEXT NOT NULL,
    completed_at    TEXT,
    config_path     TEXT,                          -- .engram/sessions/<id>.toml
    focus_topics    TEXT,                          -- JSON array
    focus_note_ids  TEXT,                          -- JSON array of seed note IDs
    focus_tags      TEXT                           -- JSON array
);

-- Dream mode outputs (speculative proposals)
CREATE TABLE dreams (
    id              TEXT PRIMARY KEY,
    agent_name      TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    content_path    TEXT,                          -- .engram/dreams/<id>.md
    dream_type      TEXT,                          -- analogy, synthesis, bridge, link
    confidence      REAL,                          -- 0.0-1.0, typically low
    promoted        BOOLEAN NOT NULL DEFAULT FALSE -- user promoted to real proposal
);

-- Predictions ledger (Predictor agent; calibration analysis is part of Predictor's job)
CREATE TABLE predictions (
    id              TEXT PRIMARY KEY,              -- ULID
    note_id         TEXT NOT NULL REFERENCES notes(id),
    excerpt         TEXT NOT NULL,                 -- the predictive sentence(s)
    claimed_at      TEXT NOT NULL,                 -- when the prediction was made
    due_at          TEXT,                          -- extracted resolution date, if any
    confidence      REAL,                          -- 0.0-1.0 if stated
    topic           TEXT,                          -- topic area for calibration grouping
    status          TEXT NOT NULL DEFAULT 'pending', -- pending, due, resolved-correct,
                                                   --   resolved-incorrect, unresolved,
                                                   --   superseded
    resolved_at     TEXT,
    resolution_note TEXT,                          -- human or agent explanation
    resolution_evidence TEXT                       -- link or quote
);
CREATE INDEX idx_predictions_status ON predictions(status);
CREATE INDEX idx_predictions_due ON predictions(due_at) WHERE due_at IS NOT NULL;

-- Flashcards (Tutor, spaced repetition)
CREATE TABLE flashcards (
    id              TEXT PRIMARY KEY,              -- ULID
    note_id         TEXT NOT NULL REFERENCES notes(id),
    question        TEXT NOT NULL,
    answer          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    -- FSRS state (free spaced-repetition scheduler)
    stability       REAL,
    difficulty      REAL,
    last_review_at  TEXT,
    next_review_at  TEXT,
    review_count    INTEGER NOT NULL DEFAULT 0,
    lapse_count     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_flashcards_due ON flashcards(next_review_at);
CREATE INDEX idx_flashcards_note ON flashcards(note_id);

CREATE TABLE flashcard_reviews (
    flashcard_id    TEXT NOT NULL REFERENCES flashcards(id),
    reviewed_at     TEXT NOT NULL,
    rating          INTEGER NOT NULL,              -- 1=again, 2=hard, 3=good, 4=easy
    PRIMARY KEY (flashcard_id, reviewed_at)
);

-- Agent actions log (every unstaged write by every agent).
-- This is the primary audit trail. git commits are made by the human, so they
-- can group multiple actions under one commit; this table preserves per-action
-- attribution regardless of how the human chooses to commit.
CREATE TABLE agent_actions (
    id              TEXT PRIMARY KEY,           -- ULID
    agent_name      TEXT NOT NULL,
    kind            TEXT NOT NULL,              -- link-add, tag-norm, note-create, ...
    files           TEXT NOT NULL,              -- JSON array of relative paths
    diff_hash       TEXT NOT NULL,              -- SHA-256 of the patch
    confidence      REAL NOT NULL,              -- 0.0-1.0, agent's self-score
    rationale       TEXT NOT NULL,              -- LLM-emitted explanation
    deliberation_id TEXT,                       -- NULL = direct (confidence-gated)
    rubric_check    TEXT NOT NULL,              -- pass | fail | n/a
    wrote_at        TEXT NOT NULL,
    -- Resolution by the human (NULL until decided)
    human_decision  TEXT,                       -- staged | rejected | amended | ignored
    decided_at      TEXT,
    -- If amended/staged, the resulting commit
    final_diff_hash TEXT,                       -- if amended, post-amend hash
    git_commit_sha  TEXT                        -- the human's commit, when staged
);
CREATE INDEX idx_agent_actions_agent ON agent_actions(agent_name);
CREATE INDEX idx_agent_actions_pending ON agent_actions(human_decision)
    WHERE human_decision IS NULL;
CREATE INDEX idx_agent_actions_wrote_at ON agent_actions(wrote_at DESC);

-- Outcome tracking (Watcher + Auditor). Beyond accept/reject: did the change last?
CREATE TABLE outcomes (
    id                  TEXT PRIMARY KEY,           -- ULID
    agent_run_id        TEXT NOT NULL REFERENCES agent_runs(id),
    note_id             TEXT REFERENCES notes(id),
    change_kind         TEXT NOT NULL,              -- create, modify, link, tag, delete, ...
    landed_at           TEXT NOT NULL,
    -- Survival checkpoints (NULL = not yet evaluated, TRUE = present, FALSE = reverted)
    survived_30d        BOOLEAN,
    survived_90d        BOOLEAN,
    survived_180d       BOOLEAN,
    -- Engagement: did the user interact with this after landing?
    visited_after       BOOLEAN NOT NULL DEFAULT FALSE,
    linked_after        BOOLEAN NOT NULL DEFAULT FALSE,
    modified_by_human   BOOLEAN NOT NULL DEFAULT FALSE,
    -- Productivity: did this change seed further work?
    seeded_note_ids     TEXT,                       -- JSON array of derived note IDs
    -- Reversal (loudest negative signal)
    reverted_at         TEXT,
    reversal_reason     TEXT
);
CREATE INDEX idx_outcomes_agent_run ON outcomes(agent_run_id);
CREATE INDEX idx_outcomes_note ON outcomes(note_id);

-- Token spend per agent (Watcher enforces budgets)
CREATE TABLE token_usage (
    agent_name      TEXT NOT NULL,
    period          TEXT NOT NULL,                  -- "2026-04" (monthly key)
    input_tokens    INTEGER NOT NULL DEFAULT 0,
    output_tokens   INTEGER NOT NULL DEFAULT 0,
    estimated_cost  REAL NOT NULL DEFAULT 0.0,      -- USD
    landings        INTEGER NOT NULL DEFAULT 0,     -- changes that landed this period
    PRIMARY KEY (agent_name, period)
);

-- Budget enforcement state
CREATE TABLE agent_budgets (
    agent_name          TEXT PRIMARY KEY,
    monthly_token_cap   INTEGER NOT NULL,
    current_period      TEXT NOT NULL,              -- "2026-04"
    paused_for_budget   BOOLEAN NOT NULL DEFAULT FALSE,
    paused_at           TEXT,
    paused_reason       TEXT
);

-- Prompt variants for shadow-mode A/B testing (Auditor's prompt evolution)
CREATE TABLE prompt_variants (
    id              TEXT PRIMARY KEY,               -- ULID
    agent_name      TEXT NOT NULL,
    variant_path    TEXT NOT NULL,                  -- agents/<name>/variants/<id>.md
    created_at      TEXT NOT NULL,
    status          TEXT NOT NULL,                  -- shadow, promoted, rejected, archived
    samples         INTEGER NOT NULL DEFAULT 0,
    -- Comparative outcome metrics vs. active prompt
    delta_acceptance REAL,                          -- +0.05 = 5pp better
    delta_survival   REAL,
    delta_cost       REAL,                          -- negative = cheaper
    promoted_at     TEXT,
    rejection_reason TEXT
);

-- Auditor recommendations (quarterly)
CREATE TABLE audits (
    id              TEXT PRIMARY KEY,               -- e.g. "2026-Q2-synthesizer"
    agent_name      TEXT NOT NULL,
    period          TEXT NOT NULL,                  -- "2026-Q2"
    created_at      TEXT NOT NULL,
    samples_read    INTEGER NOT NULL,
    recommendation  TEXT NOT NULL,                  -- keep, tune, demote, pause, retire
    rationale_path  TEXT NOT NULL,                  -- .engram/meta/audits/<id>.md
    human_decision  TEXT,                           -- accepted, rejected, deferred
    human_decided_at TEXT
);

-- External MCP clients (see 04-external-mcp.md)
CREATE TABLE mcp_clients (
    id              TEXT PRIMARY KEY,               -- ULID
    name            TEXT NOT NULL,                  -- "Travel App"
    api_key_hash    TEXT NOT NULL UNIQUE,           -- argon2 hash
    created_at      TEXT NOT NULL,
    last_used_at    TEXT,
    revoked_at      TEXT,
    scopes          TEXT NOT NULL                   -- JSON array of scope strings
);

CREATE TABLE mcp_access_log (
    id              TEXT PRIMARY KEY,               -- ULID
    client_id       TEXT NOT NULL REFERENCES mcp_clients(id),
    called_at       TEXT NOT NULL,
    tool            TEXT NOT NULL,
    args_summary    TEXT,                           -- redacted args for audit
    response_summary TEXT,                          -- redacted response for audit
    success         BOOLEAN NOT NULL,
    error_message   TEXT
);
CREATE INDEX idx_mcp_access_client_time ON mcp_access_log(client_id, called_at);

-- Corpus digestion (Curator agent; see 05-corpus-digestion.md)
CREATE TABLE corpus_digestions (
    id              TEXT PRIMARY KEY,         -- ULID
    source_path     TEXT NOT NULL,
    source_slug     TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    status          TEXT NOT NULL,            -- surveying, planned, digesting, completed, paused
    total_notes     INTEGER,
    notes_processed INTEGER NOT NULL DEFAULT 0,
    notes_kept      INTEGER NOT NULL DEFAULT 0,
    notes_discarded INTEGER NOT NULL DEFAULT 0,
    notes_archived  INTEGER NOT NULL DEFAULT 0,
    notes_merged    INTEGER NOT NULL DEFAULT 0,
    policy_path     TEXT
);

CREATE TABLE digestion_items (
    id              TEXT PRIMARY KEY,
    digestion_id    TEXT NOT NULL REFERENCES corpus_digestions(id),
    source_path     TEXT NOT NULL,            -- relative to source corpus root
    source_hash     TEXT NOT NULL,            -- SHA-256
    cluster_id      TEXT,
    initial_class   TEXT,
    disposition     TEXT,                     -- keep-evergreen-draft, keep-literature,
                                              --   merge-into:<id>, archive, discard, defer
    engram_note_id  TEXT REFERENCES notes(id),
    batch_id        TEXT,
    status          TEXT NOT NULL DEFAULT 'pending',
    decided_at      TEXT,
    rationale       TEXT
);
CREATE INDEX idx_digestion_items_digestion ON digestion_items(digestion_id);
CREATE INDEX idx_digestion_items_status ON digestion_items(digestion_id, status);

CREATE TABLE digestion_clusters (
    id              TEXT PRIMARY KEY,
    digestion_id    TEXT NOT NULL REFERENCES corpus_digestions(id),
    centroid_topic  TEXT,
    note_count      INTEGER NOT NULL,
    proposed_action TEXT,                     -- synthesize, individual, discard-cluster
    synthesis_note_id TEXT REFERENCES notes(id)
);

CREATE TABLE digestion_discards (
    digestion_item_id TEXT NOT NULL REFERENCES digestion_items(id),
    summary           TEXT NOT NULL,          -- preserved one-line summary
    discarded_at      TEXT NOT NULL,
    PRIMARY KEY (digestion_item_id)
);

-- Proposals: change drafts that need human review (high-invasiveness OR
-- below-threshold confidence OR high-invasiveness council outcomes).
-- The corresponding files live in .engram/proposals/<id>.json. The table is
-- the queryable index over those files.
CREATE TABLE proposals (
    id              TEXT PRIMARY KEY,         -- ULID
    proposing_agent TEXT NOT NULL,
    proposed_at     TEXT NOT NULL,            -- ISO 8601 UTC
    invasiveness    TEXT NOT NULL,            -- mechanical, additive, editorial, structural
    target_note_id  TEXT REFERENCES notes(id),-- NULL for new-note proposals
    proposed_diff_path TEXT NOT NULL,         -- .engram/proposals/<id>.json (full payload)
    rationale       TEXT NOT NULL,
    confidence      REAL NOT NULL,            -- agent's self-score
    deliberation_id TEXT,                     -- NULL for individual-agent proposals; set when from council
    status          TEXT NOT NULL,            -- pending, approved, rejected, expired, superseded
    decided_at      TEXT,
    decided_by      TEXT,                     -- "human" or agent name (council auto-approve path)
    resulting_action_id TEXT REFERENCES agent_actions(id) -- once approved + landed unstaged
);
CREATE INDEX idx_proposals_status ON proposals(status);
CREATE INDEX idx_proposals_target ON proposals(target_note_id);

-- Shelved proposals: contested or no-defensible-critique-found outcomes from council.
-- Files live in .engram/shelved/<id>.json with full dissent annotated.
CREATE TABLE shelved (
    id              TEXT PRIMARY KEY,         -- ULID
    proposing_agent TEXT NOT NULL,
    shelved_at      TEXT NOT NULL,
    deliberation_id TEXT NOT NULL,            -- always set; shelved means council ran
    reason          TEXT NOT NULL,            -- dissent | no_defensible_critique | timeout | budget
    summary         TEXT NOT NULL,            -- one-line summary for browse
    transcript_path TEXT NOT NULL             -- .engram/shelved/<id>.md
);
CREATE INDEX idx_shelved_at ON shelved(shelved_at DESC);

-- Per-round, per-participant votes within a council deliberation.
-- Allows reconstructing the deliberation step-by-step for audit.
CREATE TABLE deliberation_votes (
    deliberation_id TEXT NOT NULL REFERENCES deliberations(id),
    round_number    INTEGER NOT NULL,         -- 1 (CRITIQUE), 2 (post-revision CRITIQUE)
    agent_name      TEXT NOT NULL,
    vote            TEXT NOT NULL,            -- approve, request_changes, reject
    rationale       TEXT NOT NULL,
    suggested_edits_path TEXT,                -- optional file with diff
    voted_at        TEXT NOT NULL,
    PRIMARY KEY (deliberation_id, round_number, agent_name)
);

-- Pending external MCP registration requests (consent flow in flight).
CREATE TABLE mcp_register_requests (
    id              TEXT PRIMARY KEY,         -- ULID
    name            TEXT NOT NULL,
    purpose         TEXT,
    requested_scopes TEXT NOT NULL,           -- JSON array
    requested_at    TEXT NOT NULL,
    expires_at      TEXT NOT NULL,            -- consent prompts expire after 24h default
    status          TEXT NOT NULL,            -- pending, approved, denied, expired
    decided_at      TEXT,
    granted_scopes  TEXT,                     -- JSON array; subset of requested if user customized
    issued_client_id TEXT REFERENCES mcp_clients(id) -- set only on approval
);
CREATE INDEX idx_mcp_register_status ON mcp_register_requests(status);

-- Pending ask_user questions from external MCP clients.
-- Round-trip: client calls ask_user; row inserted; user is notified via Swift app
-- push channel; user replies; row updated; client polls/SSE for the answer.
CREATE TABLE pending_questions (
    id              TEXT PRIMARY KEY,         -- ULID
    client_id       TEXT NOT NULL REFERENCES mcp_clients(id),
    question        TEXT NOT NULL,
    context         TEXT,
    urgency         TEXT NOT NULL DEFAULT 'normal', -- low, normal, high
    asked_at        TEXT NOT NULL,
    expires_at      TEXT NOT NULL,            -- default 24h; configurable per call
    status          TEXT NOT NULL,            -- pending, answered, skipped, expired, muted
    answered_at     TEXT,
    answer          TEXT,                     -- user's reply text (NULL if skipped)
    user_action     TEXT                      -- reply | skip | mute_app_24h
);
CREATE INDEX idx_pending_questions_status ON pending_questions(client_id, status);

-- Coordinated flow runs (Evergreen birth ceremony, Trust ceremony, Insight harvest, etc.)
-- See 01-agents-and-council.md §Flow orchestrator + §Cost-aware planning.
CREATE TABLE flow_runs (
    id              TEXT PRIMARY KEY,         -- ULID
    flow_kind       TEXT NOT NULL,            -- evergreen_birth, trust_ceremony, insight_harvest
    target_id       TEXT,                     -- the note, agent, or quarter the flow operates on
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    current_step    INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL,            -- running, completed, blocked, failed, abandoned
    blocker_summary TEXT,
    transcript_path TEXT NOT NULL,            -- .engram/deliberations/<id>.md
    -- Cost-aware planning
    estimated_cost_usd      REAL,             -- pre-flight estimate; populated at start
    estimated_tokens_min    INTEGER,
    estimated_tokens_max    INTEGER,
    estimator_confidence    REAL,             -- 0.0-1.0; tunes whether prompt is shown
    user_confirmed_at       TEXT,             -- if estimate triggered a confirmation prompt
    actual_cost_usd         REAL,             -- accumulated as steps complete
    actual_tokens_used      INTEGER,
    midflow_pause_reason    TEXT              -- "cost_exceeded_estimate" | "user_paused" | NULL
);
CREATE INDEX idx_flow_runs_status ON flow_runs(status);

CREATE TABLE flow_step_results (
    flow_run_id     TEXT NOT NULL REFERENCES flow_runs(id),
    step_number     INTEGER NOT NULL,
    agent_name      TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    outcome         TEXT NOT NULL,            -- success, request_changes, fail, timeout, skipped
    output_path     TEXT,
    error_summary   TEXT,
    -- Cost tracking
    estimated_cost_usd  REAL,
    actual_cost_usd     REAL,
    tokens_used         INTEGER,
    PRIMARY KEY (flow_run_id, step_number)
);

-- Eval framework: per-agent benchmark suite results.
-- See 01-agents-and-council.md §Eval framework.
CREATE TABLE eval_runs (
    id              TEXT PRIMARY KEY,         -- e.g. "2026-Q3-linker-after-tuning"
    agent           TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    agent_prompt_sha TEXT NOT NULL,           -- so we know what was being tested
    agent_config_sha TEXT NOT NULL,
    model_used      TEXT NOT NULL,
    cases_run       INTEGER NOT NULL,
    cases_passed    INTEGER NOT NULL,
    total_tokens    INTEGER NOT NULL,
    total_cost_usd  REAL NOT NULL,
    aggregate_metrics TEXT NOT NULL,          -- JSON
    output_path     TEXT NOT NULL             -- .engram/evals/<agent>/runs/<id>.json
);
CREATE INDEX idx_eval_runs_agent_time ON eval_runs(agent, started_at DESC);

CREATE TABLE eval_case_results (
    eval_run_id     TEXT NOT NULL REFERENCES eval_runs(id),
    case_id         TEXT NOT NULL,
    result          TEXT NOT NULL,            -- pass | fail | error
    scores          TEXT NOT NULL,            -- JSON
    failure_reason  TEXT,
    PRIMARY KEY (eval_run_id, case_id)
);

-- Token estimator calibration: tracks actual vs. predicted to tune per-agent multipliers.
CREATE TABLE token_estimator_calibration (
    agent_name      TEXT NOT NULL,
    period          TEXT NOT NULL,            -- "2026-09" (monthly)
    calls_observed  INTEGER NOT NULL DEFAULT 0,
    sum_estimated   INTEGER NOT NULL DEFAULT 0,
    sum_actual      INTEGER NOT NULL DEFAULT 0,
    mean_error_pct  REAL,                     -- (actual - estimated) / estimated
    multiplier      REAL NOT NULL DEFAULT 1.0, -- applied to next month's estimates
    PRIMARY KEY (agent_name, period)
);

-- Embedding cache keyed by content hash + model identity.
-- Multiple model variants coexist; switching models doesn't blow away old vectors.
-- See ADR 0012.
CREATE TABLE embedding_cache (
    content_hash    TEXT NOT NULL,            -- SHA-256 of normalized embeddable text
    model           TEXT NOT NULL,            -- e.g. "bge-m3" | "text-embedding-3-large"
    model_version   TEXT NOT NULL,            -- e.g. "1.5" | "2024-01-25"
    dimensions      INTEGER NOT NULL,
    embedding       BLOB NOT NULL,            -- packed float32 vector
    first_seen_at   TEXT NOT NULL,            -- ISO 8601 UTC
    last_used_at    TEXT NOT NULL,
    use_count       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (content_hash, model, model_version, dimensions)
);
CREATE INDEX idx_embedding_cache_lru ON embedding_cache(last_used_at);
```

---

## Retrieval pipeline

Hybrid search follows the pattern from the LLM Wiki research, adapted for engram:

```
Query (from MCP tool, API, or agent)
    |
    +-- 1. BM25 via FTS5 in SQLite (exact-term matches: names, tags, IDs)
    +-- 2. Dense ANN via LanceDB HNSW (semantic similarity, optionally
    |       filtered by note_type / tags / date predicates in one pass)
    +-- 3. Reciprocal Rank Fusion to merge ranked lists
    +-- 4. Cross-encoder rerank on top 20-50 candidates
    |       (bge-reranker-v2-m3 locally, or Cohere rerank-3.5 cloud)
    +-- 5. Graph expansion: traverse outgoing/incoming wikilinks 1-2 hops
    |       from top results via SQLite recursive CTEs. The link graph IS
    |       the killer feature for a linked vault.
    +-- 6. Return top-k with metadata, snippets, and graph context
```

**Eventually-consistent vector layer.** A note modified at t=0 may not be findable via step 2 (LanceDB ANN) for up to ~500ms; the BM25 (step 1) and graph (step 5) layers cover this window so the user never sees wrong results, just slightly stale semantic search for freshly-modified notes. See [ADR 0014](adrs/0014-lancedb-vector-storage.md).

The Karpathy-style `index.md` (maintained by Cartographer) serves as an additional retrieval path: the LLM reads the index directly and navigates by following links, bypassing vector search entirely. For some queries this outperforms embeddings.

---

## Model tier mapping

Agents specify a tier, not a model. The mapping is user-configurable:

```toml
# .engram/config.toml

[models]
fast     = { provider = "anthropic", model = "claude-haiku" }
standard = { provider = "anthropic", model = "claude-sonnet" }
deep     = { provider = "anthropic", model = "claude-opus" }

[models.local]
fast     = { provider = "ollama", model = "llama3.2:3b" }
standard = { provider = "ollama", model = "llama3.2:8b" }
deep     = { provider = "ollama", model = "llama3.3:70b" }

[models.embeddings]
provider = "local"        # or "openai"
model = "bge-m3"          # local default; native dim = 1024
dimensions = 1024         # must match the model's native dimension or a
                          # supported Matryoshka-reduced size. Switching
                          # requires re-embedding via a numbered migration.
```

The `privacy: local-only` flag on a note forces the local model mapping regardless of the agent's configured tier.

---

## API surface

### REST endpoints

| Method                 | Path                              | Description                                                                              |
| ---------------------- | --------------------------------- | ---------------------------------------------------------------------------------------- |
| **Notes**              |                                   |                                                                                          |
| GET                    | `/notes`                          | List notes (filterable by type, status, tag, date, author)                               |
| GET                    | `/notes/:id`                      | Read a single note (markdown + frontmatter)                                              |
| GET                    | `/search?q=...&author=...`        | Hybrid search with provenance filter                                                     |
| GET                    | `/graph/:id`                      | Link graph neighborhood for a note                                                       |
| **Ingestion**          |                                   |                                                                                          |
| POST                   | `/ingest`                         | Upload file for ingestion                                                                |
| **Review (unstaged)**  |                                   |                                                                                          |
| GET                    | `/changes`                        | All pending unstaged agent changes (per-file, with attribution + confidence + rationale) |
| GET                    | `/changes/:path`                  | Diff + agent_actions row(s) for one file                                                 |
| POST                   | `/changes/:path/stage`            | `git add <path>` --- accept the change                                                   |
| POST                   | `/changes/:path/discard`          | `git restore <path>` --- reject the change                                               |
| POST                   | `/changes/:path/amend`            | Save edited content over the unstaged version, then stage                                |
| POST                   | `/commit`                         | `git commit` with a message (commits whatever is staged)                                 |
| **Review (proposals)** |                                   |                                                                                          |
| GET                    | `/proposals`                      | List explicit human-approval proposals (high-invasiveness)                               |
| POST                   | `/proposals/:id/approve`          | Approve --- triggers agent to write to working tree unstaged                             |
| POST                   | `/proposals/:id/reject`           | Reject a proposal                                                                        |
| **Council**            |                                   |                                                                                          |
| POST                   | `/council/query`                  | Submit a Research Council question                                                       |
| POST                   | `/council/debate`                 | Initiate Debate Mode between two notes                                                   |
| **Conversations**      |                                   |                                                                                          |
| GET                    | `/conversations`                  | List active agent conversations                                                          |
| GET                    | `/conversations/:id`              | Read conversation transcript                                                             |
| POST                   | `/conversations/:id/reply`        | Send human reply in a conversation                                                       |
| GET                    | `/conversations/:id/stream`       | SSE stream of agent's next turn (token-by-token chunks)                                  |
| POST                   | `/conversations/:id/end`          | End a conversation early                                                                 |
| **Sessions**           |                                   |                                                                                          |
| GET                    | `/sessions`                       | List goal-directed sessions                                                              |
| POST                   | `/sessions`                       | Create a new goal-directed session                                                       |
| PATCH                  | `/sessions/:id`                   | Update session (pause, resume, complete)                                                 |
| **Dreams**             |                                   |                                                                                          |
| GET                    | `/dreams`                         | List speculative proposals from dream mode                                               |
| POST                   | `/dreams/:id/promote`             | Promote a dream to a real proposal                                                       |
| DELETE                 | `/dreams/:id`                     | Dismiss a dream                                                                          |
| **Agents**             |                                   |                                                                                          |
| GET                    | `/agents`                         | List agents, their state, and trust scores                                               |
| POST                   | `/agents/:name/run`               | Manually trigger an agent                                                                |
| GET                    | `/agents/:name/memory`            | Inspect an agent's memory store                                                          |
| GET                    | `/agents/trust`                   | Trust score summary for all agents                                                       |
| **Predictions**        |                                   |                                                                                          |
| GET                    | `/predictions`                    | List predictions (filter by status, due date, topic)                                     |
| POST                   | `/predictions/:id/resolve`        | Resolve a due prediction (correct, incorrect, superseded)                                |
| GET                    | `/calibration`                    | Calibration profile (claimed vs. actual accuracy by topic)                               |
| **Pedagogy**           |                                   |                                                                                          |
| GET                    | `/flashcards/due`                 | Cards due for review today                                                               |
| POST                   | `/flashcards/:id/review`          | Submit a review rating (1-4)                                                             |
| GET                    | `/flashcards/stats`               | Review streak, retention rate, weak areas                                                |
| **Flows + cost**       |                                   |                                                                                          |
| GET                    | `/flows`                          | List active and recent flow runs                                                         |
| GET                    | `/flows/:id`                      | Flow run state, step results, cost actual vs. estimated                                  |
| POST                   | `/flows/estimate`                 | Pre-flight cost estimate for a proposed flow (`{kind, target, dry_run}`)                 |
| POST                   | `/flows/:id/confirm`              | User confirms a flow that triggered a cost prompt                                        |
| POST                   | `/flows/:id/pause`                | Pause a running flow at next step boundary                                               |
| POST                   | `/flows/:id/resume`               | Resume a blocked, paused, or failed flow                                                 |
| **Evals**              |                                   |                                                                                          |
| GET                    | `/evals/:agent`                   | List eval runs for an agent (most recent first)                                          |
| GET                    | `/evals/:agent/scorecard`         | Latest scorecard markdown                                                                |
| POST                   | `/evals/:agent/run`               | Trigger a fresh eval run (`{cases: optional case-id list}`)                              |
| GET                    | `/evals/:agent/runs/:id`          | Full results for a specific run                                                          |
| **Personal**           |                                   |                                                                                          |
| GET                    | `/biography`                      | Read the current biographer model                                                        |
| POST                   | `/voice/check`                    | Submit text, get voice-match score + suggested edits                                     |
| GET                    | `/trajectory/:concept`            | Trace thinking on a concept over time                                                    |
| **Digestion**          |                                   |                                                                                          |
| POST                   | `/digest`                         | Start a corpus digestion (`{path, policy_overrides}`)                                    |
| GET                    | `/digest`                         | List all digestions and their state                                                      |
| GET                    | `/digest/:id`                     | Status, plan, batch progress for one digestion                                           |
| GET                    | `/digest/:id/batches`             | List batches with disposition counts                                                     |
| GET                    | `/digest/:id/batches/:bid`        | Read a batch's per-item proposals (for Swift app review)                                 |
| POST                   | `/digest/:id/batches/:bid/decide` | Submit batch review (per-item approve/override/reject)                                   |
| PATCH                  | `/digest/:id/policy`              | Update policy mid-digestion                                                              |
| POST                   | `/digest/:id/recluster`           | Re-run a cluster with new policy                                                         |
| GET                    | `/digest/:id/discards`            | The discard log (one-line summaries)                                                     |
| GET                    | `/digest/:id/audit`               | Auditor's post-digestion review                                                          |
| **On-demand**          |                                   |                                                                                          |
| POST                   | `/prep`                           | Generate a conversation prep briefing                                                    |
| POST                   | `/untangle`                       | Generate a sensemaking map for a confused topic                                          |
| GET                    | `/standup`                        | Today's morning report (operational summary)                                             |
| **System**             |                                   |                                                                                          |
| GET                    | `/status`                         | System health, agent states, queue depth                                                 |
| GET                    | `/events`                         | SSE stream: agent activity, proposals, conversations, dreams                             |

### MCP servers

Engram exposes **two** MCP servers, with different audiences and trust assumptions:

- **Internal MCP** (stdio transport, full access). For Claude Desktop / Code on the user's own machine. Trusted. All tools below are available without scoping.
- **External MCP** (HTTP+SSE transport, scoped + authenticated). For the user's own client apps (e.g. a travel app). Per-client API keys, OAuth-style scopes, audit log, consent flow on first connect, privacy zones excluded by default. **See `04-external-mcp.md` for the full design.**

Both servers share the same underlying tool implementations and the same vault index. The difference is in the transport, the auth layer, and which tools are exposed by default.

### MCP tools (internal)

Exposed via the internal MCP server (stdio transport). The external MCP server exposes a curated subset plus higher-level "personal context" tools defined in `04-external-mcp.md`.

- `search_notes` --- hybrid semantic + lexical search, with optional provenance filter (`author`, `agent`, `human_only`)
- `grep_notes` --- literal pattern match (faster, more precise for known terms)
- `read_note` --- full content by ID or path
- `list_tags` --- all tags with counts
- `follow_backlinks` --- notes linking to a given note
- `follow_links` --- notes linked from a given note
- `recent_changes` --- notes modified in the last N days
- `write_note` --- create or update (gated: goes through council)
- `read_index` --- the Karpathy-style `index.md` for LLM navigation
- `list_contradictions` --- surface known contradictions from Contradiction Detector
- `list_blindspot_reports` --- surface gap reports produced by Inquirer's `blindspot` mode (quarterly negative-space analysis)
- `list_dreams` --- browse speculative proposals from dream mode
- `vault_health` --- agent trust scores, pending proposals, session status
- `read_biography` --- the user model maintained by Biographer (use to ground responses in who you're talking to)
- `trace_concept` --- diachronic view of a concept over time (the trajectory feature, exposed as a CLI command and MCP tool; not a standalone agent)
- `list_predictions` --- pending and resolved predictions, calibration data
- `due_flashcards` --- cards Tutor has scheduled for review

---

## Concurrency model

- **File watcher**: single tokio task, emits events to a channel. Reads the working tree as-is (sees both committed content and unstaged agent writes).
- **Indexer**: single consumer of file-change events. Sequential per-note processing, parallelized embedding computation. Indexes the working tree (unstaged content is searchable; user discards revert the index on next file event).
- **Agent scheduler**: one tokio task per scheduled agent. Agents run concurrently but take a per-note advisory lock (sqlite row-level) to prevent two agents from modifying the same note simultaneously. **Agents have read-write access to the working tree but NEVER call `git add` or `git commit`.** All changes land unstaged.
- **Git access**: a single dedicated tokio task owns all git operations. Agents request reads (status, diff, log) via this task; **only the API/CLI surfaces invoked by the human can issue `git add` or `git commit`.** This makes "no agent commits" enforceable at the type-system level (agents never receive a write-side git handle).
- **Council sessions**: serialized per-note (only one council may discuss a given note at a time), parallelized across different notes.
- **Conversations**: one active conversation per agent at a time. Human replies arrive via API; agent responses generated asynchronously.
- **Dream mode**: single background tokio task, lowest priority. Yields immediately when any foreground agent needs to run. Uses `fast` model tier to minimize resource contention.
- **Scout pollers**: one tokio task per configured feed. Independent of each other. Feed checks are staggered to avoid burst traffic.
- **Goal-directed sessions**: not a separate thread --- a priority bias applied to existing agent scheduler tasks. Active sessions boost the scheduling priority of relevant agents.
- **API server**: standard axum multi-threaded handler. SSE connections managed by broadcast channel.
- **MCP server**: single connection (Claude Desktop connects one session).
- **Pacekeeper**: continuously monitors the unstaged-change queue depth and time-to-review, throttling producing agents when the human is overwhelmed.

---

## `.engram/` directory structure

The `.engram/` directory is engram's working area within the vault. Some contents are git-tracked (deliberations, proposals --- they're knowledge artifacts), others are not (index, artifacts, dreams).

```
.engram/
├── config.toml                     # vault config, providers, privacy zones
├── index.sqlite                    # derived metadata + FTS5 + graph (NOT in git, rebuildable)
├── vectors/                        # LanceDB datasets (per ADR 0014; backed up
│   └── notes_v1.lance/             #   but technically rebuildable from
│       ├── data/                   #   embedding_cache + vault)
│       └── _versions/
├── artifacts/                      # ingested files, content-addressed (NOT in git)
│   └── <sha256-prefix>/
│       └── <sha256>.<ext>
├── deliberations/                  # council transcripts (in git)
│   └── 2026-04-15-0003.md
├── proposals/                      # pending human-review changes (in git)
│   └── <deliberation-id>.md
├── shelved/                        # contested proposals with dissent (in git)
│   └── <deliberation-id>.md
├── dreams/                         # speculative outputs from dream mode (NOT in git)
│   └── <dream-id>.md
├── sessions/                       # goal-directed session configs (in git)
│   └── attention-paper.toml
├── meta/                           # system reports (in git)
│   ├── biography.md                # Biographer's user model
│   ├── voice-model.md              # Voice Keeper's learned voice
│   ├── predictions.md              # Predictor's ledger
│   ├── calibration.md              # Predictor's calibration profile
│   ├── health.md                   # Watcher's weekly report
│   ├── audits/                     # Auditor's quarterly evaluations (in git)
│   │   └── 2026-Q2-synthesizer.md
│   ├── trajectories/               # outputs of the diachronic trace feature (one per concept)
│   │   └── attention.md
│   ├── prep/                       # Conversation Prep briefings
│   │   └── 2026-04-17-alice.md
│   ├── untangling/                 # Untangler sensemaking maps
│   │   └── 2026-04-16-rag-vs-fine-tuning.md
│   └── fact-check-2026-04.md       # Fact Checker's monthly report
├── sidecar/                        # per-note rich metadata (in git, pretty JSON)
│   └── <note-id>.json              # provenance history, embedding, agent visits, etc.
├── flashcards/                     # Tutor's spaced-repetition cards (in git)
│   └── <note-id>.md
├── witness/                        # Witness's private acknowledgments (NOT in git)
│   └── 2026-04-16.md
├── digestion/                      # Curator's corpus digestion state (in git)
│   └── notes-2022-03/              # one subdir per source corpus (slug)
│       ├── plan.md                 # survey + initial classification
│       ├── policy.toml             # curation policy (user-editable)
│       ├── batches/                # per-batch proposal records
│       │   └── 2026-04-17-001.md
│       ├── discards.md             # one-line summaries of discarded notes
│       └── audit.md                # Auditor's post-digestion review
└── logs/                           # structured agent run logs (NOT in git)
    └── 2026-04-15.jsonl
```

Also note that **`notes/archive/`** holds verbatim-preserved corpus content that the user wanted to keep but not actively curate (`type: archive`). Most agents skip archive notes; only Cartographer indexes them.

---

## Backup and disaster recovery

The vault is canonical. Git provides versioning, not durability --- a lost `.git` directory is a lost vault. A knowledge tool you depend on for years requires an explicit backup story.

### What to back up

**Always:**

- Vault root (markdown files, `.git/`)
- `.engram/sidecar/` (per-note metadata, git-tracked anyway)
- `.engram/deliberations/`, `.engram/proposals/`, `.engram/shelved/` (council artifacts, in git)
- `.engram/sessions/`, `.engram/meta/` (configs, reports, in git)
- `.engram/flashcards/` (Tutor cards, in git)
- `.engram/digestion/` (Curator state, in git)
- `.engram/config.toml`

**Conditional:**

- `.engram/artifacts/` --- the raw ingested files. Often large (PDFs, audio). Either back up locally OR rely on a separate content-addressed remote (S3-compatible) keyed by SHA-256.
- `.engram/vectors/` --- LanceDB datasets. **Technically rebuildable** from `embedding_cache` (in SQLite) + the vault (re-embed any missing). Backing it up directly is recommended because rebuild at 10K notes takes ~10 min; at 100K it's substantial. Use directory snapshot semantics — LanceDB datasets are multi-file with internal versioning.

**Never:**

- `.engram/index.sqlite` --- derived; rebuildable via `engram reindex --full`.
- `.engram/dreams/` --- speculative; not durable record.
- `.engram/witness/` --- intentionally on-device only; never synced anywhere.
- `.engram/logs/` --- ephemeral.

### Recommended topology

Three-layer defense:

1. **Git remote.** Push the vault to a private GitHub/GitLab/self-hosted git server. Frequent (post-commit hook). Cheap. Survives local-machine loss.
2. **Filesystem snapshot.** macOS Time Machine, Linux btrfs/ZFS snapshots, or restic to a cloud target. Daily. Survives filesystem corruption and accidental destructive commits.
3. **Artifact remote** (if `artifacts/` is large). S3-compatible bucket synced via rclone, keyed by SHA-256. Decouples large media from git history.

### Backup Watcher agent

A simple agent (added in v1) that monitors:

- Time since last successful git push (`git log origin/main..HEAD` count)
- Time since last filesystem snapshot (Time Machine API on macOS, file mtime on snapshot dirs elsewhere)
- Reachability of the artifact remote (probe + last-sync timestamp)

Surfaces warnings via the Swift app status indicator and standup report when any layer is stale (configurable thresholds; defaults: git remote stale > 24h, filesystem snapshot stale > 7d, artifact remote stale > 7d).

The Backup Watcher does not perform backups. It monitors them. The user (or their existing tooling) does the actual backup. This keeps engram out of the credentials business.

### Recovery procedure

Documented in the Backup Watcher's quarterly report:

1. Clone vault from git remote.
2. Restore `.engram/artifacts/` from artifact remote (or accept that ingested raw files are gone but literature notes remain).
3. Run `engram serve` --- it detects missing index and runs `engram reindex --full` automatically.
4. Verify by checking `engram status` for note count, artifact count, and any "missing artifact" warnings.

Recovery time on a 10K-note vault: clone (seconds), reindex including embedding (~10 minutes on Apple Silicon), artifact restore (depends on size and bandwidth). Targeted at one hour total.

---

## Secrets management

Engram needs outbound credentials for cloud LLM providers (Anthropic, OpenAI), embedding services (OpenAI), and optionally object-storage backends (S3, Backblaze, etc.). These cannot live in plaintext config.

### Storage hierarchy (in order of preference)

1. **macOS Keychain** (default on macOS) via the `security-framework` crate. One keychain item per provider, namespaced `engram.<provider>`. Read on `engram serve` startup; cached in process memory.
2. **Linux Secret Service** (GNOME Keyring, KWallet) via the `keyring` crate when on Linux. Same namespacing.
3. **Encrypted file** (`age`-encrypted at `.engram/secrets.age`) for headless servers without a keyring daemon. Decrypted with a passphrase prompted at startup.
4. **Environment variables** (`ENGRAM_ANTHROPIC_API_KEY`, etc.) for development and CI. Lowest precedence; explicit override behavior.

### What's stored

- Cloud LLM provider keys (Anthropic, OpenAI)
- Embedding service keys (if cloud)
- Cohere rerank key (if used)
- Object-storage credentials (S3, etc.)
- Git push credentials (only if engram triggers backups; otherwise out of scope --- the user's git config holds these)

**Never stored** by engram secrets:

- External MCP client API keys (those are issued by engram, hashed in `mcp_clients` table, given to the client once at registration)
- User Apple Calendar / iCloud / etc. credentials (handled by Swift app frameworks)

### CLI

```bash
engram secrets set anthropic            # prompts for value, stores in keychain
engram secrets list                     # names only, never values
engram secrets rotate anthropic         # prompts for new value, atomic swap
engram secrets remove anthropic
engram secrets export --to-env-file     # for backup or migration; warned about
```

### Rotation policy

Documented but not enforced:

- Provider keys rotate annually or on suspected compromise.
- External MCP client keys: revoke + reissue (no automatic rotation; client must re-register).

### Audit

Every secret access is logged (which provider, which agent, when --- never the value) to `.engram/logs/secrets.jsonl`. Rotated weekly.

---

## Schema migrations

Engram's SQLite schema and sidecar JSON schema will evolve. Old vaults must continue to work after engram is upgraded.

### Migration system

**Numbered SQL migrations** in `crates/engram-index/migrations/`:

```
001_initial.sql
002_add_predictions.sql
003_add_outcomes.sql
004_agent_actions.sql
...
```

`sqlx`-style runner: on `engram serve` startup, the index manager checks the `schema_migrations` table, applies any pending migrations in order, records each in the table. Failures abort startup; the user can roll back via `engram migrate --rollback <N>` for the last N migrations (only when the migration provided a documented down-script; not all do).

**Forward-only by default.** Every migration is designed to be safely applied to any prior version. Down-scripts are best-effort.

### Sidecar JSON migrations

Sidecar files carry `schema_version: <int>` (already in the schema). On read, if the version is old, the loader applies in-memory upgrades. On write, sidecars are rewritten at the current version.

If a sidecar version is _newer_ than the current binary supports (e.g., user downgraded), the loader refuses and surfaces an explicit error rather than silently corrupting.

### Stability promise

> Migrations forward, never break old vaults.

Concretely:

- Major version bumps may add tables/columns and may transform sidecar JSON shape; never drop user data.
- Old vaults can always be opened by a newer engram (after migrations apply).
- Newer vaults cannot be opened by an older engram (sidecar version check refuses).

### Derived state

`notes_fts`, `links`, `tags`, agent memory caches, LanceDB vector datasets --- all rebuildable. `engram reindex --full` drops and rebuilds (SQLite tables via SQL; LanceDB datasets via re-upsert from `embedding_cache` + computed embeddings for any missing). Migrations to derived-state tables can be aggressive (drop + recreate) without data risk.

**Real state** --- `agent_actions`, `predictions`, `outcomes`, `corpus_digestions`, `mcp_clients`, sidecar JSONs --- requires careful preservation. Migrations to these always include a data-copy step, never a drop-and-recreate.

### CLI

```bash
engram migrate              # apply all pending; default on `engram serve` startup
engram migrate --status     # show applied vs. pending
engram migrate --to <N>     # apply through migration N (for staged upgrades)
engram migrate --rollback N # undo last N migrations (only if reversible)
engram migrate --dry-run    # show what would happen
```

---

## System-wide cost ceiling

Per-agent token budgets exist (see `agent_budgets` table); they prevent any single agent from running away. They do not prevent the _aggregate_ from exceeding what the user expects to spend.

### Configuration

In `.engram/config.toml`:

```toml
[cost]
monthly_usd_cap = 50.0          # hard cap; system pauses when exceeded
warning_threshold = 0.75        # warn the user at 75%
provider_cost_table = "default" # token-to-USD conversion source

[cost.alert]
notify_swift_app = true
include_in_standup = true
```

### Enforcement

The Watcher tracks aggregate spend across all agents per calendar month. When monthly spend reaches:

- **75% of cap:** emit a warning event (Swift app notification, included in next standup).
- **100% of cap:** **system-wide pause.** All scheduled agents stop. On-demand agent calls (Untangler, Research Council, etc.) error with "monthly cost cap reached; raise via `engram config set cost.monthly_usd_cap <new>` or wait for month rollover."
- **Embeddings, indexing, and local-model work** continue regardless --- they don't incur LLM cost.

### Visibility

Cost dashboard in the Swift app and `engram status` shows:

- Month-to-date spend (USD)
- Per-agent breakdown (top 5)
- Sparkline of daily spend
- Days remaining vs. percent of cap consumed
- Projection for end of month

### Rationale

The user must never be surprised by their bill. Per-agent budgets answer "is any single agent runaway?" --- the system-wide cap answers "is the whole system on track for the month?" Both are needed. Cost-per-landing (already tracked by Watcher) provides the value-side metric to evaluate whether the spend is producing real work.

---

## Error handling and resilience

### Error taxonomy

All errors implement a typed hierarchy via `thiserror`:

```rust
pub enum EngramError {
    Transient(TransientError),    // retryable: network, 5xx, rate limits
    Permanent(PermanentError),    // user-actionable: validation, not-found, auth
    System(SystemError),          // engram bugs: schema mismatch, panic, invariant violation
    External(ExternalError),      // wrapped third-party errors with domain context
}
```

Sub-types categorize precisely (`ProviderRateLimit`, `ProviderServerError`, `ContextOverflow`, `KeychainUnavailable`, `VaultLocked`, `SchemaMismatch`, etc.). The agent runner inspects the type to decide retry vs. propagate.

### LLM call retry policy

Every LLM call goes through a single retry wrapper:

```
attempt 1: immediate
attempt 2: backoff = 1s × (2^0) ± jitter (50-150%)
attempt 3: backoff = 1s × (2^1) ± jitter
attempt 4: backoff = 1s × (2^2) ± jitter
max delay between attempts: 30s
total wall-clock budget per call: 60s (hard cap)
```

Jitter is multiplicative (uniform in [0.5, 1.5]) to prevent thundering-herd when multiple agents recover from the same provider outage simultaneously.

Retry is conditional on error type:

| Error class          | Retry?                                              |
| -------------------- | --------------------------------------------------- |
| Network timeout      | Yes (transient)                                     |
| 5xx                  | Yes (transient)                                     |
| 429 rate limit       | Yes, respect `Retry-After` header if present        |
| 4xx (other)          | No (permanent — bad request, auth, etc.)            |
| Context overflow     | No (re-prompt would just fail again; surface error) |
| Schema parse failure | One retry with stricter prompt; then fail           |

### Circuit breaker (per provider)

A per-provider circuit breaker prevents cascading failures when a provider is down:

```
state: closed | open | half_open

closed:
  count failures over rolling 60s window
  if failures >= 10 in 60s OR consecutive_failures >= 5: -> open
open:
  reject all calls immediately (return CircuitBreakerOpen error)
  after cooldown (30s default): -> half_open
half_open:
  allow 1 trial call
  if success: -> closed (reset counters)
  if failure: -> open (cooldown 2x previous)
```

Cooldown doubles each consecutive open transition up to 5min ceiling, then plateau. State is per-provider; if Anthropic is down, OpenAI is unaffected. State is in-memory (ephemeral; reset on restart).

When circuit is open, agents receive `CircuitBreakerOpen`. Mechanical agents skip; thinking agents queue their work for later. The Pacekeeper is notified and may pause non-essential agents.

### Global timeouts

Every external call has a hard timeout at the connection layer. Defaults (overridable per-provider in config):

- LLM calls: 60s (matches per-call retry budget)
- Embedding calls: 30s
- Web fetches (Scout, Fact Checker): 15s connect, 30s body
- MCP outbound: 10s
- Git remote operations: 60s
- All sqlite operations: 5s (failsafe; expected sub-second)

Hitting a timeout produces a `Transient` error and routes through the retry wrapper.

### Streaming structured output with early-exit

LLM responses for agent calls are structured JSON conforming to the agent's output schema. The naïve flow is: send request → wait for full response → parse JSON → validate → use. This wastes tokens on responses that will be rejected:

- Agent self-reports `confidence: 0.4` early in the response, but we keep generating the rest of the JSON (rationale paragraph, proposed actions array) before discarding the whole thing because confidence is below threshold.
- Tier escalation will retry the call at a higher tier anyway; the rest of the low-tier output is dead weight.

**Streaming + incremental JSON parsing solves this.** All LLM provider implementations support streaming (`stream: true` in the request); engram uses it for every agent call.

```rust
// engram-llm pseudocode
async fn call_streamed<O: AgentOutput>(
    prompt: PromptStructured,
    agent: &AgentConfig,
) -> Result<O> {
    let mut buffer = String::new();
    let mut json_parser = IncrementalJsonParser::new::<O>();
    let mut stream = provider.stream(prompt).await?;

    while let Some(chunk) = stream.next().await {
        buffer.push_str(&chunk?);

        // Try to extract whatever fields have arrived so far.
        // Order in the output schema: confidence, rationale, action_payload.
        // The schema directive at the top of the prompt instructs models to
        // emit `confidence` first.
        if let Some(partial) = json_parser.extract_known_fields(&buffer)? {
            // Early-exit: confidence below threshold and not yet at ceiling tier
            if let Some(conf) = partial.confidence {
                if conf < agent.early_exit_confidence_floor {
                    stream.cancel().await?;
                    return Err(EngramError::EarlyExit {
                        reason: "confidence below floor",
                        partial,
                    });
                }
            }
        }
    }

    // Final parse + validate
    json_parser.finalize(&buffer)
}
```

**Schema discipline for streaming.** Every agent's structured output schema MUST place `confidence` first, then `rationale`, then any payload fields (proposed links, edits, etc.). The agent prompt instructs the model to emit fields in this order. This enables early-exit on the cheap fields before the expensive payload generates.

**Per-agent floor.** `early_exit_confidence_floor` is configurable in `agents/<name>/config.toml` (default 0.3 — well below typical thresholds; we cancel only when the agent itself signals "definitely no good"). Conservative by design — false-positive cancellations are wasteful, but rare at this floor.

**Token savings.** For agents with high rejection rates (Heretic in particular), early-exit saves ~30-60% of output tokens on the rejected calls. Less dramatic but real for Linker and Gardener.

**Compatibility with tiered escalation (ADR 0011).** Early-exit on low confidence triggers immediate escalation — no need to wait for the rest of the cheap-tier output before retrying at the higher tier. Total wall-clock latency for a `fast → standard` escalation drops from ~6s to ~3s.

### Request coalescing (council retrieval)

When the council convenes 3-5 agents on the same note (Synthesizer + Devil's Advocate + Linker + Cartographer all evaluating "this proposed evergreen"), each agent calls the same retrieval functions: `read_note(target_id)`, `hybrid_search(target_content)`, `list_neighbors(target_id, depth=2)`. Without coalescing, these are 3-5× duplicated calls.

**Coalescer:** A short-lived in-memory request deduplicator at the retrieval-API boundary. Identical requests within a 50ms window share a single response.

```rust
// engram-index pseudocode
struct RequestCoalescer<K, V> {
    in_flight: Arc<Mutex<HashMap<K, broadcast::Receiver<V>>>>,
    window: Duration,  // 50ms default
}

async fn fetch_coalesced<K: Hash, V: Clone>(
    coalescer: &RequestCoalescer<K, V>,
    key: K,
    fetcher: impl Future<Output = V>,
) -> V {
    let mut in_flight = coalescer.in_flight.lock().await;
    if let Some(rx) = in_flight.get(&key) {
        let mut rx = rx.resubscribe();
        drop(in_flight);
        return rx.recv().await.unwrap();
    }
    let (tx, rx) = broadcast::channel(8);
    in_flight.insert(key.clone(), rx);
    drop(in_flight);

    let value = fetcher.await;
    tx.send(value.clone()).ok();
    // remove key after window so a slightly-later request triggers a fresh fetch
    tokio::spawn(async move {
        sleep(coalescer.window).await;
        coalescer.in_flight.lock().await.remove(&key);
    });
    value
}
```

**Coalesced calls** (in the order of value):

| Call                   | Key                          | Window | Notes                                               |
| ---------------------- | ---------------------------- | ------ | --------------------------------------------------- |
| `read_note(id)`        | `note_id`                    | 50ms   | Highest hit rate during council                     |
| `read_sidecar(id)`     | `note_id`                    | 50ms   | Pairs with read_note                                |
| `hybrid_search(q,n)`   | `(query_hash, n)`            | 200ms  | Embedding + BM25 + RRF; expensive                   |
| `list_neighbors(id,d)` | `(note_id, depth)`           | 100ms  | Pure sqlite; coalesce mainly to reduce DB pressure  |
| `read_index()`         | `()`                         | 1s     | Whole-vault index; rare update                      |
| `embed(text)`          | `(content_hash, model, ver)` | n/a    | Already deduplicated via embedding cache (ADR 0012) |

**Latency win:** council convene-to-quorum-ready time drops from ~600ms (5 sequential retrievals × ~120ms each) to ~200ms (one batch + cached responses). User-perceived faster diff queue updates.

**Cost win:** retrieval calls don't typically cost LLM tokens (BM25 + sqlite + local embed lookup), but `hybrid_search` triggers reranker calls in some configurations; coalescing prevents duplicate reranker LLM calls. Real if Cohere rerank is enabled.

**Correctness:** the coalescer serves identical-key requests from one fetch. Different-key requests go through normally. The window is short enough that staleness is bounded; longer windows would hurt freshness.

### Graceful degradation when a provider is down

When the cloud provider's circuit breaker opens:

1. Agents configured for the affected provider are paused for the cooldown window.
2. If the agent has a `fallback_provider` in its config (e.g., `fallback_provider = "ollama"`), the runner tries the fallback. Quality may be lower; this is logged as a `degraded_provider` event.
3. If no fallback, the agent's pending work queues; the user is notified once via Swift app: "Anthropic appears down; X agents paused, will resume automatically."
4. Mechanical agents (Linker low-confidence checks) silently degrade; thinking agents (Synthesizer, Heretic) silently defer.

### Atomic writes (markdown + sidecar + sqlite; LanceDB is downstream)

A single agent action may need to update three **strictly-consistent** places: the markdown file, the sidecar JSON, and the SQLite index. **LanceDB (the vector store) is downstream and eventually consistent** (per [ADR 0014](adrs/0014-lancedb-vector-storage.md)) — its updates happen async after the strict-consistency triple commits.

Pattern: **temp-file rename + write-ahead log entry + async LanceDB upsert.**

```
1. Begin: append a row to write_intents (sqlite) with:
     intent_id, agent_id, target_path, target_sidecar, expected_diff_hash
2. Write markdown to <path>.tmp (filesystem-atomic on macOS via O_DIRECT optional)
3. Write sidecar to <sidecar>.tmp
4. Write sqlite updates inside a transaction, including an `agent_actions`
   row referencing the intent_id, and an `embedding_cache` entry if the
   embedding has been computed (or scheduled)
5. fsync both .tmp files
6. Rename <path>.tmp -> <path> (atomic on POSIX)
7. Rename <sidecar>.tmp -> <sidecar> (atomic on POSIX)
8. Commit sqlite transaction (sqlite WAL ensures durability)
9. Mark intent as committed in write_intents
10. Async: enqueue LanceDB upsert (best-effort; retried by reconciliation if it
    fails); the new vector becomes ANN-searchable typically within ~500ms
```

On startup, the runner scans `write_intents` for non-committed rows and:

- If both .tmp files exist: replay step 6-8.
- If one .tmp file is missing: roll back (delete the other .tmp; clear the intent).
- If both .tmp files are missing but intent is uncommitted: clear the intent (the write never happened).

**LanceDB reconciliation** runs separately on startup and hourly: for any SQLite `notes` row whose `(id, content_hash)` doesn't match a LanceDB record, queue an upsert. This catches missed async upserts from crashed prior runs.

This guarantees the **markdown + sidecar + sqlite triple is strictly consistent**, modulo the one-rename-then-crash window (sub-millisecond on a healthy filesystem). The LanceDB vector layer is eventually consistent — semantic search on freshly-modified notes may lag by ~500ms but never returns _wrong_ results (only stale-by-one-version results); BM25 + graph layers cover this window in the retrieval pipeline.

### Per-note advisory lock

To prevent two agents from modifying the same note simultaneously, the runner takes a per-note advisory lock from a `note_locks` sqlite table:

```sql
CREATE TABLE note_locks (
    note_id    TEXT PRIMARY KEY,
    holder     TEXT NOT NULL,        -- agent name + run id
    acquired_at TEXT NOT NULL,
    expires_at  TEXT NOT NULL        -- 5min default; auto-expire prevents zombie locks
);
```

Acquisition: `INSERT ... ON CONFLICT DO NOTHING` returning rowcount. If 0, the note is locked; the agent's runner backs off (jittered, 1-5s) and retries up to 3 times before deferring the run.

Sub-agent invocation (e.g., Curator → Synthesizer): the parent's lock is **inherited** via a `parent_holder` field. Sub-agents check both the lock holder and any inherited holders; same-tree access is allowed. Different-tree access blocks.

Expiration: locks expire after 5min. The runner checks expiration on acquire and reaps expired locks. A panicking agent that never released its lock won't deadlock the system.

### Indexer behavior on `git restore`

When the user runs `git restore <path>` to discard an unstaged agent change, `notify-rs` emits a file-modified event. The indexer:

1. Re-reads the file from disk (now matching HEAD).
2. Parses frontmatter; if the `id:` is unchanged, treats this as a content update.
3. Updates the `notes`, `notes_fts`, and `links` rows (SQLite). Queues a LanceDB upsert for the new content hash (eventually consistent; reconciles within ~500ms).
4. Updates the corresponding `agent_actions` row's `human_decision` to `rejected` (the runner's git-watcher detects the diff disappeared).
5. The sidecar is **not** rolled back automatically — it's git-tracked and the user may also `git restore` it. If the sidecar diverges from the markdown's actual state, the next agent visit reconciles it.

### Hot-reload semantics for agent prompts

When a file in `agents/<name>/` changes (detected by the `agents/` watcher):

- **In-flight runs continue with the prompt they started with.** No mid-run swap.
- **Council deliberations in progress** continue with the prompts that were loaded at council convene time. The new prompts apply to the next council session.
- **The runner reloads** prompts at the start of each new agent run.
- **Schema validation** runs on reload: if the new `config.toml` is invalid, the reload is rejected with an error visible in the next standup; the agent continues with the previous valid config.
- **Prompt evolution variants** (in `agents/<name>/variants/`) trigger a separate code path; promotion of a variant is a file move and follows the same hot-reload semantics.

### `agent_actions` ↔ git stage reconciliation

The user runs `git add <path>` (via Swift app, CLI, or directly). How does `agent_actions.human_decision` update from NULL to `staged`?

**Mechanism:** the engram process watches `.git/index` via `notify-rs` (in addition to watching the working tree). On any change to `.git/index`:

1. Runs `git diff --cached --name-only` to get the staged paths.
2. For each staged path, finds the most recent `agent_actions` row with `files` containing that path and `human_decision IS NULL`.
3. Updates `human_decision = 'staged'` and `decided_at = now()`.
4. After commit (also detected via index change → HEAD update), runs `git rev-parse HEAD` and writes `git_commit_sha` to all rows updated in this batch.

**Edge cases:**

- If the user manually edits a file before staging (`amend` flow): the diff hash on disk no longer matches `agent_actions.diff_hash`. The runner records `human_decision = 'amended'` and stores the post-amend hash in `final_diff_hash`.
- If the user stages a file that has no corresponding pending `agent_actions` row (a manual edit): no row is created or updated — manual edits are not in the agent action log by design.
- If the user reverts a previously-staged-and-committed change later: a new commit is created (`git revert`) and a synthetic `agent_actions` row is **not** created; the revert is just a normal commit attributed to the human. Watcher's outcome tracking notices the file now mismatches the prior committed state and updates the prior `agent_actions` row's `outcome` field.

### Sidecar writes and `agent_actions.files`

Each sidecar write counts as a file write. When an agent updates a note's content, both the markdown file and the sidecar file appear in `agent_actions.files` as a JSON array:

```json
"files": ["notes/evergreen/attention.md", ".engram/sidecar/01JRZK3M7P.json"]
```

This is intentional — the user reviewing `git diff` will see both changes; the action log reflects both.

### Rate limiting

**REST API:** per-IP and per-API-token rate limits, enforced by `tower_governor` middleware:

| Endpoint group        | Default rate limit                |
| --------------------- | --------------------------------- |
| `/changes`            | 60 req/min per source             |
| `/notes/*`, `/search` | 120 req/min per source            |
| `/ingest`             | 30 req/min per source             |
| `/council/*`, `/prep` | 10 req/min per source (LLM-heavy) |
| `/agents/:name/run`   | 6 req/min per source              |

Limits are configurable in `config.toml`. Limits apply per-source where source = remote IP for unauthenticated calls, or `client_id` for authenticated external MCP calls.

**External MCP:** in addition to REST limits, per-client per-tool quotas:

| Tool                 | Default quota       |
| -------------------- | ------------------- |
| `personal_context`   | 100/day per client  |
| `preferences`        | 200/day per client  |
| `recent_thinking_on` | 500/day per client  |
| `ask_user`           | 20/day per client   |
| `record_session`     | 50/day per client   |
| `search_notes`       | 1000/day per client |

Exceeding a quota returns 429 with `Retry-After` set to seconds-until-midnight-UTC. Quotas reset at 00:00 UTC.

### argon2 parameters

External MCP API keys are stored hashed with argon2id. Parameters:

```
algorithm: argon2id
m_cost: 19456 KiB (19 MiB)   -- OWASP 2024 baseline
t_cost: 2 (iterations)
p_cost: 1 (parallelism)
salt_len: 16 bytes (random per-key)
hash_len: 32 bytes
```

These parameters are stored alongside the hash so they can evolve over time without invalidating existing keys (verify-with-old-params, then rehash-with-new-params on next access).

### TLS and transport security

External MCP runs over HTTP+SSE, by default unencrypted on `localhost:7878`. **TLS is explicitly out of scope for v1** — the threat model assumes the local machine is trusted. For users who need remote access, the recommended path is Tailscale (which provides E2E encryption transparently) or a reverse proxy with TLS termination (caddy, nginx).

This is documented in `09-threat-model.md`. v2+ may add native TLS for cloud-relay scenarios.

### UTC and user-timezone convention

**All timestamps in SQLite are ISO 8601 UTC.** Format: `2026-04-17T14:32:00Z`. No exceptions. The schema enforces this via column comments; deserialization rejects non-UTC.

The user's local timezone lives in `.engram/config.toml`:

```toml
[user]
timezone = "America/Los_Angeles"   # IANA identifier; auto-detected on first run
```

Used for:

- "Morning standup" — fires at user-local 06:00
- "Today's flashcards due" — uses user-local midnight as the day boundary
- "Predictions due today" — same
- "Annual Review" — fires at user-local 23:00 on December 31
- FSRS `next_review_at` calculation
- Any user-facing date display in the Swift app

Internal scheduling, logs, and database timestamps remain UTC. Conversion happens at the rendering boundary (Swift app) or the scheduler boundary (cron-style triggers).

### Configuration management

Single source of truth: `.engram/config.toml`. Per-agent overrides in `agents/<name>/config.toml`. Per-corpus-digestion overrides in `.engram/digestion/<slug>/policy.toml`.

**Schema validation on load.** All TOML files are parsed via `serde` into typed structs with `#[serde(deny_unknown_fields)]`. Unknown fields fail loudly. Default values are documented in the struct definition (single source — no scattered defaults).

**Hot-reload scope:**

| What                                                  | Reload behavior                             |
| ----------------------------------------------------- | ------------------------------------------- |
| `agents/<name>/prompt.md`                             | Hot-reload (next run picks up)              |
| `agents/<name>/config.toml`                           | Hot-reload (next run; in-flight unaffected) |
| `.engram/config.toml` --- agent set, schedule changes | Hot-reload (scheduler refreshed)            |
| `.engram/config.toml` --- model providers, secrets    | Restart required                            |
| `.engram/config.toml` --- privacy zones               | Hot-reload (applies to next file event)     |
| `.engram/digestion/<slug>/policy.toml`                | Hot-reload (applies to next batch)          |

Reload-required-but-not-applied state is surfaced in `engram status`.

### Dependency security

- **`cargo audit`** runs in CI on every PR and on a daily scheduled job. Failures block merge.
- **`cargo deny`** enforces a license policy and a banned-crates list. License policy: MIT, Apache-2.0, BSD-2/3, ISC, CC0, Unicode-DFS-2016. AGPL, GPL, and unspecified licenses are blocked (engram's own license posture is "all rights reserved" per the project README; permissive deps only).
- **Pinning policy:** `Cargo.lock` is committed for the binary crate. Library crates use SemVer ranges. Major version bumps require a PR with explanation and re-running cargo audit/deny.
- **SBOM generation:** `cargo cyclonedx` produces a CycloneDX SBOM at release time, attached to each release.
- **Crate selection:** prefer fewer, well-maintained crates over many marginal ones. Each new dependency requires justification in PR description.

### Backup verification (not just monitoring)

The Backup Watcher monitors backup _recency_ (when did we last back up?) but recency is not the same as recoverability. **Quarterly restore drill:**

A scripted task (`engram backup verify`) performs a full restore-and-validate against a sandbox:

1. Clone the git remote into a temp directory.
2. Restore artifacts from the artifact remote (if configured) into the sandbox `.engram/artifacts/`.
3. Run `engram serve` against the sandbox vault on a non-default port.
4. Trigger `engram reindex --full`.
5. Run a smoke-test query suite (e.g., 20 known queries with expected result fingerprints).
6. Diff `agent_actions` row counts and `notes` row counts against a pre-flight snapshot.
7. Tear down the sandbox.

Output: `meta/backup-verifications/YYYY-MM-DD.md` with pass/fail per step. Backup Watcher surfaces failures in standup. The user is responsible for running this quarterly; engram nags if the most recent verification is > 90 days old.

### RTO and RPO

- **RTO (Recovery Time Objective):** 1 hour for a 10K-note vault. Measured: clone (seconds) + artifact restore (depends on bandwidth, typically 10-30 min) + reindex (~10 min on Apple Silicon).
- **RPO (Recovery Point Objective):** 24 hours, assuming nightly git push and Time Machine. With more aggressive backup (post-commit hook git push), effectively 0.

### Alerting framework

**v1 alerting is intentionally minimal:** only Backup Watcher staleness and cost-cap warnings surface as Swift-app notifications. Users get a daily standup summarizing system health.

**v2+** introduces an alert table:

```sql
CREATE TABLE alerts (
    id          TEXT PRIMARY KEY,
    severity    TEXT NOT NULL,   -- info, warn, critical
    category    TEXT NOT NULL,   -- backup, cost, agent_health, provider, security
    title       TEXT NOT NULL,
    body        TEXT,
    raised_at   TEXT NOT NULL,
    acknowledged_at TEXT,
    resolved_at TEXT
);
```

Plus an Alerter agent (v2.1) that aggregates Watcher signals, Auditor findings, and circuit-breaker state into a unified alert stream surfaced in the Swift app.

### Correlation IDs

Every logical operation gets a correlation ID at its origin point:

- **Capture-originated:** the Swift app generates a ULID at submit time and includes it in the `Idempotency-Key` header. The server propagates it through ingest → extract → Scribe → review queue.
- **Agent-originated:** the agent runner generates a ULID per agent invocation. It propagates through all sub-agent calls, council deliberations, file writes, and `agent_actions` rows.
- **MCP-originated:** every external MCP tool call gets a ULID, logged in `mcp_access_log` and propagated to any downstream work it triggers.

Correlation IDs appear in every `tracing` span, in the structured JSON log lines, and in the `agent_actions` and `outcomes` tables. The 2am-debug story: "why did Synthesizer write this paragraph?" → grep correlation ID across logs and tables → reconstruct the full chain.

### Core failure modes (legacy summary)

- **LLM call failures:** see retry/circuit-breaker policy above.
- **Index corruption:** `engram reindex --full` rebuilds from vault. No data loss possible since vault is canonical.
- **Working-tree conflicts:** agents take per-note advisory locks to prevent concurrent writes; on rare lock failures the agent run is deferred and retried.
- **Extraction failures:** artifact preserved, status set to `extraction_failed`, user notified. Manual retry available via `engram reextract <artifact-hash>`.
- **Agent panics:** caught at the runner level via `catch_unwind`. Run logged as `panicked` in `agent_runs`. Watcher alerts. The runner remains healthy for other agents.
- **`engram serve` panics:** systemd / launchd restarts the process. State is durable (sqlite + filesystem); the only loss is in-flight in-memory work, which is bounded by the per-call timeout.
