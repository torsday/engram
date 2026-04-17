# Agent Specification Template

## Purpose

`01-agents-and-council.md` describes each agent's job, trigger, output, invasiveness, and tools at a conceptual level. That's right for design discussion. **It is not enough to implement an agent.** Implementation requires: a concrete prompt skeleton, a structured output JSON schema, a confidence-formula, a tool list with input/output types, and test fixtures.

This doc defines the **agent specification template** every agent must complete before implementation, plus filled specs for the **five v1 agents** (Linker, Gardener, Cartographer, Scribe, Ingestor).

The template enforces consistency. A developer (or Claude Code) implementing any agent should be able to take the spec and produce a working implementation without architectural decisions left undecided.

This doc also defines the **v1 proposal-without-council format** --- since council deliberation is v1.1, v1 proposals work as standalone JSON files reviewed in the Swift app.

---

## Template

Every agent specification is one page covering eight sections:

```markdown
# Agent: <Name>

## Identity
- **Name:** <kebab-case agent name; matches agents/<name>/ directory>
- **Tier:** maintenance | processing | structural | thinking | personal | temporal | pedagogical | external | meta | on-demand
- **Phase:** v1 | v1.1 | v1.2 | v1.3 | v2 | v2.1 | v2.2 | v3+
- **Model tier:** fast | standard | deep
- **Max invasiveness:** mechanical | additive | editorial | structural

## Prompt skeleton
The system prompt template, with `{{placeholder}}` slots for runtime context.
Includes: role, goals, constraints, output-format directive, confidence-rating instruction.

## Structured output schema
JSON schema for the agent's structured output. Always includes `confidence` (0.0-1.0)
and `rationale` (one paragraph). Tool-call schemas if the agent uses tools.

## Confidence formula
How the per-action confidence value is computed. May be the LLM self-score alone,
or a weighted combination with retrieval-signal agreement.

## Tools
List of tools the agent may call. Each tool has: name, input type, output type,
purpose. References definitions in `03-architecture.md`.

## Triggers
What causes this agent to run. References scheduler config or event sources.

## Outputs
What the agent produces (note files, sidecar updates, proposals, conversation turns,
etc.). References file locations and formats.

## Test fixtures
Reference to the integration-test fixture set that exercises this agent.
Located at `tests/fixtures/agents/<name>/`.
```

---

## Linker

### Identity
- **Name:** `linker`
- **Tier:** maintenance
- **Phase:** v1
- **Model tier:** `fast`
- **Max invasiveness:** `additive`

### Prompt skeleton

```markdown
You are Linker, an agent in the engram knowledge system. Your job: find missing
wikilinks between notes. Propose connections that would help the user navigate
their vault.

# Context
- User biography (if available): {{biography_excerpt}}
- Note being analyzed:
  Title: {{note.title}}
  Type: {{note.type}}
  ID: {{note.id}}
  Body excerpt:
  {{note.body_excerpt}}

- Top {{neighbors.count}} semantically similar notes (from hybrid retrieval):
  {{neighbors.list_with_titles_and_excerpts}}

- Existing wikilinks in this note:
  {{existing_outgoing_links}}

# Constraints
- Propose at most 5 new wikilinks per call.
- Do NOT propose a link to a note already in `existing_outgoing_links`.
- Each proposed link must reference a real note ID from the `neighbors` list.
- Confidence calibration: rate honestly. The system rewards calibration, not
  optimism. Watcher tracks claimed vs. actual acceptance.

# Output
Return ONLY a JSON object matching the LinkerOutput schema. No prose outside the JSON.
```

### Structured output schema

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "type": "object",
  "required": ["confidence", "rationale", "proposed_links"],
  "properties": {
    "confidence": {
      "type": "number",
      "minimum": 0.0,
      "maximum": 1.0,
      "description": "Self-assessed confidence that ALL proposed links are good."
    },
    "rationale": {
      "type": "string",
      "description": "One paragraph: what made these links promising and what could be wrong."
    },
    "proposed_links": {
      "type": "array",
      "maxItems": 5,
      "items": {
        "type": "object",
        "required": ["target_id", "anchor_text", "insertion_context"],
        "properties": {
          "target_id":         {"type": "string", "description": "ULID of target note"},
          "anchor_text":       {"type": "string", "description": "Display text for the wikilink"},
          "insertion_context": {"type": "string", "description": "Sentence in source note where link belongs"}
        }
      }
    }
  }
}
```

### Confidence formula

```
confidence_final = 0.5 × confidence_llm_self_score
                 + 0.3 × retrieval_agreement_score
                 + 0.2 × calibration_adjustment

retrieval_agreement_score:
  for each proposed link: did BM25 + dense + graph all rank target_id in top-N?
  score = (matches_top_5 / 3.0)  -- 1.0 if all three retrieval methods agreed
  averaged across all proposed links

calibration_adjustment:
  if Watcher has < 30 historical decisions: 1.0 (no adjustment)
  else: clamp(historical_acceptance_rate / claimed_average_confidence, 0.5, 1.5)
  -- agents that overstate get pulled down; agents that understate get pulled up
```

### Tools

| Tool                   | Input                          | Output                  | Purpose                          |
| ---------------------- | ------------------------------ | ----------------------- | -------------------------------- |
| `hybrid_search`        | `{query: str, limit: int}`     | `Vec<Neighbor>`         | Find semantically similar notes  |
| `read_note`            | `{id: str}`                    | `Note`                  | Read a candidate target note     |
| `list_outgoing_links`  | `{id: str}`                    | `Vec<LinkRef>`          | Get current links from this note |

### Triggers

- File-change event on any note (debounced 30s)
- Scheduled weekly sweep on notes Linker has not visited in > 60 days

### Outputs

- **Auto-land path** (confidence ≥ threshold): inline wikilink insertion in the markdown file at `insertion_context`, plus an HTML-comment provenance marker `<!-- by: linker confidence: 0.93 -->`. Sidecar `agent_visit_log` updated. `agent_actions` row inserted.
- **Proposal path** (confidence < threshold): a v1 proposal file (see "Proposal-without-council format" below) and a `proposals` table row.

### Test fixtures

`tests/fixtures/agents/linker/`:
- `obvious-link/` --- two notes with high mutual relevance; expected: high-confidence link proposed
- `redundant/` --- target already linked; expected: no proposal
- `low-signal/` --- weak retrieval agreement; expected: low confidence → proposal not auto-land
- `wrong-target/` --- agent should reject a plausible-but-incorrect target
- `voice-keeper-blocking/` --- proposed link's anchor text doesn't match user voice; expected: Voice Keeper participation in v1.1+

---

## Gardener

### Identity
- **Name:** `gardener`
- **Tier:** maintenance
- **Phase:** v1
- **Model tier:** `fast`
- **Max invasiveness:** `editorial` (for cleanup; deletions go through proposal)

### Prompt skeleton

```markdown
You are Gardener, an agent in the engram knowledge system. Your job: prune
stale content from the vault. Specifically:

1. Remove dead wikilinks (links to notes that no longer exist).
2. Remove resolved TODOs (`- [x]` items, or text where the action is clearly done).
3. Flag (do not remove) notes that have decayed below the evergreen rubric:
   no recent links, no recent edits, no incoming references.

# Context
Note being analyzed:
  Title: {{note.title}}
  Type: {{note.type}}
  Status: {{note.status}}
  Last modified: {{note.modified_at}}
  Incoming link count: {{note.incoming_link_count}}
  Body:
  {{note.body}}

# Dead-link candidates (already verified as dead by the runner):
  {{dead_links.list}}

# TODO candidates (already extracted by the runner):
  {{todo_candidates.list_with_context}}

# Constraints
- Never propose deleting a whole note; flag it instead.
- Never modify text other than removing dead links / resolved TODOs.
- For TODO removal: only remove if the surrounding text makes clear the item is done.
  When in doubt, leave it.
- Confidence calibration: rate honestly.

# Output
Return ONLY a JSON object matching the GardenerOutput schema.
```

### Structured output schema

```json
{
  "type": "object",
  "required": ["confidence", "rationale", "removals", "flags"],
  "properties": {
    "confidence":  { "type": "number", "minimum": 0.0, "maximum": 1.0 },
    "rationale":   { "type": "string" },
    "removals": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["kind", "location", "before_text", "after_text"],
        "properties": {
          "kind":        { "enum": ["dead_link", "resolved_todo"] },
          "location":    { "type": "string", "description": "Heading + line range" },
          "before_text": { "type": "string" },
          "after_text":  { "type": "string" }
        }
      }
    },
    "flags": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["note_id", "reason"],
        "properties": {
          "note_id": { "type": "string" },
          "reason":  { "enum": ["stale", "orphaned", "decayed_evergreen"] }
        }
      }
    }
  }
}
```

### Confidence formula

```
For each removal:
  dead_link: confidence = 0.99 (deterministic; runner pre-verified)
  resolved_todo: confidence = LLM self-score
  averaged across all removals
```

Flags don't carry confidence (they're advisory, not actions).

### Tools

| Tool                  | Input             | Output                | Purpose                            |
| --------------------- | ----------------- | --------------------- | ---------------------------------- |
| `verify_link_alive`   | `{target_id: str}`| `bool`                | Confirm a link target exists       |
| `list_incoming_links` | `{id: str}`       | `Vec<LinkRef>`        | Backlinks for staleness assessment |

### Triggers

- Scheduled daily at 03:00 user-local
- File-change event on a note containing `- [` (TODO syntax) → focused TODO scan only

### Outputs

- **Removals (auto-land path):** edit markdown in place; HTML-comment provenance.
- **Flags:** add `engram/needs-review` tag with `reason` in the comment; never auto-removed.

### Test fixtures

`tests/fixtures/agents/gardener/`:
- `dead-link/` --- a wikilink to a deleted note; expected: removed
- `live-link/` --- a wikilink to an existing note; expected: untouched
- `done-todo/` --- a TODO followed by clear "done" context; expected: removed
- `unresolved-todo/` --- a TODO with no clear resolution; expected: untouched
- `stale-note/` --- 2-year-old note with no incoming links; expected: flagged not deleted

---

## Cartographer

### Identity
- **Name:** `cartographer`
- **Tier:** maintenance
- **Phase:** v1 (continuous mode); v1.3 adds quarterly tag-audit mode
- **Model tier:** `fast` (continuous); `standard` (audit)
- **Max invasiveness:** `editorial` (MOC/index updates); `structural` (tag renames, audit mode only)

### Prompt skeleton (continuous mode)

```markdown
You are Cartographer, an agent in the engram knowledge system. Your continuous
job: maintain the Karpathy-style index.md and any active MOC notes. The index
gives every note in the vault one sentence of context, sorted for navigation.

# Context
- Recent note changes (last 24h):
  {{recent_changes.list_with_titles}}

- Existing index.md head (first 200 lines):
  {{current_index_head}}

- Notes missing from current index:
  {{missing_notes.list}}

- Notes in index but no longer in vault:
  {{orphaned_index_entries.list}}

# Constraints
- Index entries are exactly one line each: `- [[Title]]: <one-sentence summary>`
- Sort by: type (evergreen first, then literature, then MOC, then archive).
  Within type: alphabetical by title.
- One-sentence summaries should be ≤ 20 words and capture the note's core claim.
- Confidence calibration: rate honestly per-update.

# Output
Return ONLY a JSON object matching the CartographerContinuousOutput schema.
```

### Structured output schema (continuous mode)

```json
{
  "type": "object",
  "required": ["confidence", "rationale", "index_updates"],
  "properties": {
    "confidence":  { "type": "number" },
    "rationale":   { "type": "string" },
    "index_updates": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["op", "title"],
        "properties": {
          "op":      { "enum": ["add", "update", "remove"] },
          "title":   { "type": "string" },
          "summary": { "type": "string" }
        }
      }
    }
  }
}
```

### Confidence formula

LLM self-score weighted by:
- For `add` / `update`: did the summary use words present in the note? (+0.1 if yes; bias against hallucinated summaries)
- For `remove`: did the runner pre-verify the target note no longer exists? (deterministic; confidence 0.99)

### Tools

| Tool                 | Input             | Output       | Purpose                              |
| -------------------- | ----------------- | ------------ | ------------------------------------ |
| `read_note`          | `{id: str}`       | `Note`       | Read note for summarization          |
| `read_index`         | `{}`              | `String`     | Current `index.md` contents          |
| `list_notes`         | `{filters: ...}`  | `Vec<NoteRef>` | Full vault listing for missing-detection |

### Triggers

- File-change event on notes (creates/deletes/title changes); debounced 60s
- Scheduled hourly index re-render

### Outputs

- Modified `index.md` (auto-land for additive/updates; council in v1.1+ for removal-on-active-note)

### Test fixtures

`tests/fixtures/agents/cartographer/`:
- `new-note-add/` --- verify added to index in correct position
- `note-removed/` --- verify removed from index
- `title-renamed/` --- verify summary preserved on title change
- `bad-summary/` --- verify low-confidence summary triggers proposal not auto-land

---

## Scribe

### Identity
- **Name:** `scribe`
- **Tier:** processing
- **Phase:** v1
- **Model tier:** `fast`
- **Max invasiveness:** `editorial` (fleeting notes); `additive` (literature notes; never edits user content)

### Prompt skeleton

```markdown
You are Scribe, an agent in the engram knowledge system. Your job: clean up
fleeting notes (quick captures, voice transcripts, share-sheet drops) so they
become readable without changing their meaning.

# Context
Note being cleaned:
  Type: {{note.type}}
  Source: {{note.source}}     -- e.g. "voice-memo", "share-sheet", "type"
  Captured at: {{note.captured_at}}
  Body:
  {{note.body}}

# Cleanup operations allowed
- Fix obvious transcription errors (e.g., "two" -> "to" in clear context)
- Add paragraph breaks where speech run-on muddles structure
- Fix capitalization at sentence starts
- Remove filler words ("um", "like", "you know") IF the surrounding context
  doesn't depend on them
- Add a title in frontmatter if missing (use first sentence as guide)
- Normalize tags to existing namespace (`topic/foo` not `Topic Foo`)

# Constraints
- NEVER change the meaning. If you would rephrase a thought into different words
  expressing a different idea, leave it alone.
- NEVER add content the user did not say.
- NEVER change `type:` or `id:` frontmatter.
- For literature notes: format only; do not editorialize.
- Confidence calibration: rate honestly.

# Output
Return ONLY a JSON object matching the ScribeOutput schema.
```

### Structured output schema

```json
{
  "type": "object",
  "required": ["confidence", "rationale", "cleaned_body", "frontmatter_updates"],
  "properties": {
    "confidence":   { "type": "number" },
    "rationale":    { "type": "string" },
    "cleaned_body": { "type": "string", "description": "Full replacement body" },
    "frontmatter_updates": {
      "type": "object",
      "additionalProperties": { "type": "string" }
    }
  }
}
```

### Confidence formula

LLM self-score weighted by:
- Length-similarity check: `cleaned_body` should be 80-110% of original by character count for fleeting; 95-105% for literature. Outside this band, confidence cap at 0.7 (likely substantive change, not just cleanup).
- Edit-distance check: word-level Levenshtein ratio > 0.6 (more than 40% of words changed) caps confidence at 0.5.

### Tools

| Tool                 | Input             | Output       | Purpose                            |
| -------------------- | ----------------- | ------------ | ---------------------------------- |
| `list_existing_tags` | `{}`              | `Vec<String>`| Used for tag normalization         |

### Triggers

- File-change event on notes with `type: fleeting` or `type: literature` (debounced 30s)
- Newly created notes (post-Ingestor for literature, post-capture for fleeting)

### Outputs

- Modified note body (auto-land if confidence ≥ threshold; proposal otherwise)
- Frontmatter updates merged into existing frontmatter

### Test fixtures

`tests/fixtures/agents/scribe/`:
- `voice-memo-raw/` --- transcript with um/like/run-on; expected: cleaned, confidence high
- `dense-prose/` --- already-clean writing; expected: minimal changes, confidence high
- `meaning-change-attempt/` --- input that tempts rephrasing; expected: low confidence, proposal
- `literature-cleanup/` --- formatting only; expected: no editorial changes

---

## Ingestor

### Identity
- **Name:** `ingestor`
- **Tier:** processing
- **Phase:** v1 (text, markdown, web URLs, PDFs via Claude vision, images via vision); audio in v1.2
- **Model tier:** `standard` (extraction); `fast` (classification)
- **Max invasiveness:** `structural` (creates new literature notes); always proposes — never auto-land

### Prompt skeleton (classification stage)

```markdown
You are Ingestor's classifier. Given a file, identify what it is so the right
extractor pipeline runs.

# Input
- Filename: {{file.name}}
- MIME type: {{file.mime}}
- File size: {{file.size_bytes}}
- First 1KB (text-decoded if possible): {{file.preamble}}

# Categories
- academic_paper: scholarly article, journal paper, preprint
- article: news, blog post, magazine
- book_chapter: extracted from a book
- screenshot: rendered text content from a screen
- diagram: information-bearing image (chart, flowchart, photo of whiteboard)
- voice_memo: short personal audio (< 5 min)
- podcast: long-form audio (>= 5 min)
- video: any video
- web_page: HTML or web-saved content
- document: Word, RTF, plain text
- raw_text: text without obvious structure
- unknown: cannot determine

# Output
{
  "classification": "<one of the categories>",
  "confidence": 0.0-1.0,
  "rationale": "<one sentence>"
}
```

### Prompt skeleton (extraction stage; example for academic_paper via Claude vision)

```markdown
You are Ingestor's extractor for academic papers. Read this PDF and extract
the structured content needed to draft a literature note.

# Input
[The PDF is attached as a vision input.]

# What to extract
- Title (exact)
- Authors (exact)
- Publication year
- Source URL or DOI if visible
- Abstract (if present; otherwise generate a 3-sentence summary)
- 5-10 key claims, one sentence each
- 2-3 notable quotes with their context (≤ 50 words each)
- Citation count to other works (just count, not the references)

Do NOT editorialize. Do NOT speculate beyond what the paper states.

# Output
Return ONLY a JSON object matching the AcademicPaperExtraction schema.
```

### Structured output schemas

Multiple, one per extractor. Each includes `confidence` and `rationale`. Schema details in `engram-extract` crate; the extraction stage's schema is dispatched on classification verdict.

### Confidence formula

```
For classification:
  confidence = LLM self-score, no adjustment

For extraction:
  confidence_final = 0.7 × confidence_llm_self_score
                   + 0.3 × structural_completeness
  where structural_completeness = (filled_required_fields / total_required_fields)
```

### Tools

Extractors are not "tools" in the LLM-tool-call sense; they are deterministic dispatchers. The LLM gets the file content (text or vision); the runner orchestrates.

### Triggers

- `POST /ingest` from any source (Swift app, CLI, external MCP via `record_session`, internal MCP via `write_note`)

### Outputs

- Artifact stored content-addressed at `.engram/artifacts/<sha256>.<ext>`
- Literature note draft at `notes/literature/<slug>.md` (with sidecar)
- **Always proposed**, never auto-landed. The user reviews via diff queue.
- `artifacts` row inserted in sqlite

### Test fixtures

`tests/fixtures/agents/ingestor/`:
- `academic-paper.pdf/` --- expected: literature note with title/authors/abstract/claims
- `screenshot-of-tweet.png/` --- expected: literature note with attribution
- `web-article.html/` --- expected: readability extraction + literature note
- `corrupted.pdf/` --- expected: extraction failure, status `extraction_failed`, artifact preserved
- `huge-file.pdf` (50MB) --- expected: queued; backpressure if many concurrent

---

## v1 Proposal-without-council format

In v1 the council deliberation engine is not yet built. But proposals must still work — every Linker low-confidence wikilink, every Ingestor literature note, every Cartographer questionable rename needs to enter a review queue.

The v1 proposal is **a standalone JSON file** at `.engram/proposals/<id>.json` plus a row in the `proposals` SQLite table. The Swift app reads the table, displays each proposal with a diff preview, and accepts/rejects/edits.

### File schema

```json
{
  "schema_version": 1,
  "id": "01JRZK7N2P...",
  "proposing_agent": "linker",
  "proposed_at": "2026-04-17T10:23:00Z",
  "invasiveness": "additive",
  "target_note_id": "01JRZK3M7P...",
  "rationale": "Strong semantic + graph signal: 5 hops between [[Attention]] and [[Compression]] but high BM25 + dense agreement.",
  "confidence": 0.74,

  "proposed_diff": {
    "kind": "edit",
    "files": [
      {
        "path": "notes/evergreen/attention.md",
        "before_sha256": "...",
        "after_content": "...full file content after change..."
      },
      {
        "path": ".engram/sidecar/01JRZK3M7P.json",
        "before_sha256": "...",
        "after_content": "...full sidecar after change..."
      }
    ]
  },

  "expires_at": "2026-05-17T10:23:00Z",
  "status": "pending"
}
```

### Lifecycle

```
created (status=pending) -> awaiting human action
human approves: status=approved
  -> the runner applies the diff to the working tree (UNSTAGED)
  -> creates an agent_actions row referencing the proposal id
  -> proposal file moves to .engram/proposals/approved/<id>.json (audit trail)
human rejects: status=rejected
  -> proposal file moves to .engram/proposals/rejected/<id>.json
  -> the proposing agent's memory records the rejection (rejection_ttl_days)
expires (status=expired)
  -> after expires_at; same as rejected for memory purposes
superseded (status=superseded)
  -> if the underlying note changes such that the diff no longer applies cleanly,
     the proposal is superseded; the agent gets a chance to re-propose
```

### Approval mechanics in the Swift app

The Swift app's diff-review surface (described in `03-architecture.md` §Swift app) presents the proposed diff exactly as it will appear if approved. Approval triggers `POST /proposals/:id/approve`. The runner then applies the diff via the WriteGit handle (which writes files; the user still has to `git add` separately), creates the `agent_actions` row, and updates the proposal's status.

This is the v1 path. In v1.1, council deliberation runs *before* a proposal lands here, often producing automatic resolution (LAND or SHELVE) so the proposal queue becomes shorter and only contains things the council couldn't decide automatically.

---

## How agents are added

To add a new agent:

1. Write the spec using this template. Save it adjacent to the existing five (or in a new doc grouped by tier).
2. Create `agents/<name>/` directory with `prompt.md` (use the spec's prompt skeleton) and `config.toml`.
3. Implement the structured output schema as a Rust type in `engram-agents` crate.
4. Implement the confidence formula as a function in the same crate.
5. Add test fixtures at `tests/fixtures/agents/<name>/`.
6. Run integration tests against the fixtures.
7. Enable in `.engram/config.toml`.

Steps 3 and 4 are the only Rust code changes. Steps 1, 2, and 5 are pure data (per ADR 0002).

---

## Why this template exists

Without filled specs, "implement Linker" was estimated at multi-day exploratory work in the v1 feasibility review. With filled specs, "implement Linker" is one or two focused days: the prompt is written, the schema is defined, the confidence formula is testable, the tools are listed, the fixtures specify expected behavior. Same for the other four v1 agents.

This is the bridge from design to implementation. The template applies equally to v1.1+ agents when their phase arrives.
