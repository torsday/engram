# Note Conventions

## Purpose

Notes in engram serve two audiences with different needs:

- **Humans** read and write in Obsidian. They want clean filenames in the sidebar, lean frontmatter that folds away, free-form prose, native plugin compatibility, and no machine clutter polluting the reading experience.
- **Agents** read, write, and tend in the Rust core. They want stable references, predictable structure, queryable metadata, conventional anchor points for surgical insertions, and unambiguous attribution.

These needs only conflict if you put everything in one place. The trick is **layering**: the markdown file stays human-first; agent-rich metadata lives in a sidecar; conventional structure is optional but reliable when present. Done right, the two audiences never compete.

This document is the single reference for filename rules, frontmatter schema, sidecar contents, folder layout, conventional sections, tag namespaces, link/block-id conventions, and provenance markers. Every agent prompt and every human onboarding pulls from here.

---

## The two layers

| Layer                                  | Where it lives                          | Audience              | Properties                                       |
| -------------------------------------- | --------------------------------------- | --------------------- | ------------------------------------------------ |
| Markdown file (frontmatter + body)     | `notes/<type>/<slug>.md`                | Human-first           | Lean, readable, foldable, plugin-compatible      |
| Sidecar JSON                           | `.engram/sidecar/<id>.json`             | Agent-first           | Rich, structured, git-tracked, never rendered    |

The note's identity (its ID) ties the two together. Wikilinks and titles operate on the markdown layer; agent reasoning operates on both.

---

## Filename rules

**Pure title-slug. No IDs, hashes, or dates in filenames.**

```
notes/evergreen/attention-as-lossy-compression.md
notes/literature/attention-is-all-you-need.md
notes/fleeting/2026-04-17-shower-thought.md         # date allowed for inherently-temporal types
notes/journal/2026-04-17.md                          # date for journals
```

Slugging rules (applied automatically when an agent or the Swift app creates a note; users in Obsidian use whatever filename they want and the system normalizes on save):

- Lowercase
- Spaces → hyphens
- Strip punctuation except hyphens
- Collapse consecutive hyphens
- Truncate to 80 characters
- Strip leading/trailing hyphens

### Collision handling

When a writer attempts to create a note whose slug already exists, **Cartographer** (which owns navigation and naming) appends the smallest disambiguator that resolves the conflict:

```
attention-as-lossy-compression.md         # exists
attention-as-lossy-compression-2.md       # second note with same slug
attention-as-lossy-compression-3.md       # third
```

Most notes will never collide. The disambiguator is added at write time only when needed --- existing notes are never renamed to add `-1`. The disambiguator is purely positional and carries no meaning.

When the user manually creates a colliding filename in Obsidian, the file watcher detects the collision on the next index pass and Cartographer proposes a rename (which the user can accept, override, or ignore via tagging the file `engram/keep-name`).

### Renames

The note's `id` in frontmatter is canonical. When a file is renamed (in Obsidian, by an agent, or via shell), the file watcher detects the rename via the surviving ID:

1. File at path A (with ID X) disappears.
2. File at path B (with ID X) appears.
3. The watcher recognizes this as a rename, not a delete-plus-create.
4. The link graph in the index is updated; wikilinks pointing at A are repaired (Obsidian also handles this natively for vault-internal renames).
5. Sidecar at `.engram/sidecar/X.json` is unaffected --- it's keyed by ID, not path.

---

## Frontmatter schema

**Lean. Human-relevant only. Foldable in Obsidian without missing anything important when scanning.**

### Required fields (all note types)

```yaml
---
id: 01JRZK3M7PQNX8B...        # ULID. Canonical. Never changes.
title: Attention as lossy compression
type: evergreen                 # see type list below
---
```

### Common optional fields

```yaml
status: evergreen               # draft | candidate-evergreen | evergreen | needs-review | contested
created: 2026-04-15             # ISO date
tags:                           # array of slash-namespaced tags
  - topic/attention
  - topic/information-theory
aliases:                        # array of alternative titles for wikilink resolution
  - Attention is lossy compression
  - Attention as compression
```

### Type-specific fields

**Literature notes** add source information:

```yaml
type: literature
source_url: https://arxiv.org/abs/1706.03762
authors: ["Vaswani et al."]
published: 2017
```

**Heretical notes** declare what they argue against:

```yaml
type: heretical
challenges: 01JRZK3M7P...       # ID of the note this contradicts
```

**Archive notes** preserve their source corpus path:

```yaml
type: archive
source_corpus: notes-2022-03    # short slug; full provenance in sidecar
```

### Allowed `type:` values

| Type           | Purpose                                         |
| -------------- | ----------------------------------------------- |
| `fleeting`     | Quick captures, voice memos, share-sheet drops  |
| `literature`   | One per ingested source                         |
| `evergreen`    | Curated, atomic concept notes                   |
| `moc`          | Maps of content / index                         |
| `archive`      | Corpus-digestion preserved (read-only, inert)   |
| `journal`      | Personal/dated entries                          |
| `heretical`    | Sustained counter-argument to an evergreen      |
| `deliberation` | Council transcript (in `.engram/`)              |

### What does NOT belong in frontmatter

Anything an agent generates or maintains that is not a thing a human reading the note cares about:

- `created_by`, `ingested_by`, `ingested_at` (move to sidecar)
- `birth_certificate`, `deliberation_id` (move to sidecar)
- `provenance_history`, `agent_visit_log` (move to sidecar)
- `embedding_*`, `rubric_check_*` (move to sidecar)
- `source_artifact` hash, `source_path`, `source_hash` (move to sidecar; `source_url` stays for human navigation)

> **Note on the SQLite index.** The `notes` and `links` tables in `index.sqlite` carry a `created_by` column that mirrors the sidecar's provenance. This is a deliberate denormalization for query speed (e.g., "show all evergreen notes authored by Synthesizer"). The sidecar is the durable record; the SQLite column is derived and rebuilds on `engram reindex --full`. Likewise for `created_at` / `modified_at` (mirror filesystem mtime + sidecar provenance).

The principle: **if a human reading the note in Obsidian wouldn't care about it, it goes in sidecar.**

---

## Sidecar JSON

**Location:** `.engram/sidecar/<id>.json`

**Tracked in git.** Sidecars are durable record, not derived state. They travel with the vault across clones and survive `engram reindex`.

**Format:** pretty-printed JSON, one field per line. Diff-readable. The unstaged-diff review workflow depends on humans being able to read agent changes, so binary formats are off the table.

### Schema sketch

```json
{
  "id": "01JRZK3M7PQNX8B...",
  "schema_version": 1,

  "created_by": "synthesizer",
  "birth_certificate": "2026-04-15-0003",

  "provenance_history": [
    { "event": "created", "by": "synthesizer", "at": "2026-04-15T14:32:00Z", "deliberation": "2026-04-15-0003" },
    { "event": "linked", "by": "linker", "at": "2026-04-15T15:01:00Z", "confidence": 0.93 },
    { "event": "probed", "by": "socratic-prober", "at": "2026-04-16T09:12:00Z" }
  ],

  "embedding": {
    "model": "bge-m3",
    "version": "1.5",
    "dimensions": 1024,
    "hash": "sha256:abc123...",
    "computed_at": "2026-04-15T14:32:30Z"
  },

  "agent_visit_log": [
    { "agent": "linker", "at": "2026-04-17T03:00:00Z", "outcome": "no-change" },
    { "agent": "fact-checker", "at": "2026-04-20T03:00:00Z", "outcome": "verified" }
  ],

  "rubric_check_history": [
    { "at": "2026-04-15T14:32:00Z", "result": "pass", "by": "socratic-prober" }
  ],

  "calibration_claims": [
    { "claim": "transformers will plateau by 2027", "confidence": 0.7, "by": "human", "extracted_by": "predictor" }
  ],

  "ingestion": {
    "via": "ingestor",
    "at": "2026-04-15T10:30:00Z",
    "source_artifact": "a3f4e2...sha256",
    "source_corpus": null
  }
}
```

Fields are populated by the agents that own them. Empty/null fields are omitted to keep diffs small.

### Sidecar growth and pruning

Sidecars grow as agents visit notes. To keep them git-friendly, the **Gardener** runs a quarterly pass:

- Trims `agent_visit_log` to the last 100 entries per agent.
- Trims `rubric_check_history` to the last 50 entries.
- Provenance history (`provenance_history`, `birth_certificate`, `created_by`) is **never** pruned --- it's durable record.

Typical sidecar size: 2--10 KB. Bounded growth; readable diffs.

---

## Folder structure

**Flat by type at the top. Flat inside each type.**

```
notes/
├── fleeting/        # quick captures
├── literature/      # one per source
├── evergreen/       # the curated core; flat, no subdirectories
├── moc/             # maps of content (Cartographer maintains)
├── archive/         # corpus-digestion preserved (preserves source structure inside)
└── journal/         # date-organized
```

**Folders are for type. Tags and links are for topic.** No `evergreen/Programming/Languages/Rust/` --- that's tag and MOC work.

This matches the Matuschak/Zettelkasten convention and is dramatically easier for agents (predictable paths, no "where does this go" decision per write).

Exceptions:
- `archive/` preserves the source corpus's internal structure (so `notes-2022-03/2. KNOWLEDGE/foo.md` lands at `archive/notes-2022-03/2. KNOWLEDGE/foo.md`).
- `journal/` may have year subfolders (`journal/2026/2026-04-17.md`) if the user prefers; engram doesn't enforce.

---

## Conventional sections

Don't enforce a rigid template. Instead, define **section names that agents look for** but tolerate absence. Humans write free-form prose; agents have known places to find and append structured content.

| Section          | Owner / writer            | Purpose                                          |
| ---------------- | ------------------------- | ------------------------------------------------ |
| `## Sources`     | Source Demand, human      | Citations and reference list                     |
| `## Connections` | Linker (proposes), human  | Wikilinks to related concepts                    |
| `## Probe`       | Socratic Prober           | Stress-test questions before evergreen promotion |
| `## Challenged by` | Heretic                 | Links to heretical counter-notes                 |
| `## Open questions` | Inquirer, human        | Unresolved questions about this concept          |
| `## Predictions` | Predictor                 | Extracted prediction claims                      |

Rules:

- A note can have any of these sections, all of them, or none. None is the default.
- When an agent needs to add content, it adds to the conventional section, **creating it if missing**.
- Agents append; they never overwrite content above the agent-attribution comment in a section without going through council.
- Humans can rename sections, and agents will create a new one rather than fight. (Cartographer audits and proposes consolidation if useful.)

---

## Tags

Hierarchical via `/` (Obsidian-native). Reserved namespaces for agents.

### User-owned namespaces

| Namespace            | Purpose                                                  |
| -------------------- | -------------------------------------------------------- |
| `topic/<concept>`    | Subject classification (`topic/attention`)               |
| `area/<life-area>`   | Domain (`area/work`, `area/personal`, `area/research`)   |
| `type/<note-type>`   | Mirror of `type:` frontmatter (for tag-pane navigation)  |
| `status/<state>`     | Mirror of `status:` frontmatter                          |
| `<freeform>`         | Anything else the user wants (`interesting`, `revisit`)  |

### Reserved `engram/` namespace (agent writes only)

| Tag                           | Set by                  | Meaning                                              |
| ----------------------------- | ----------------------- | ---------------------------------------------------- |
| `engram/needs-citation`       | Source Demand           | Note has uncited factual claims                      |
| `engram/needs-confidence`     | Confidence Annotator    | Note has unmarked confidence claims                  |
| `engram/contested`            | Council                 | Has at least one shelved-with-dissent deliberation   |
| `engram/has-heresy`           | Heretic                 | A heretical counterpart note exists                  |
| `engram/needs-review`         | Various                 | Agent flagged for human attention                    |
| `engram/keep-name`            | User                    | Cartographer should not propose renaming this file   |
| `engram/private`              | User                    | Excluded from external MCP regardless of zone        |

The `engram/` namespace is reserved --- humans should not write tags there. Agents only write within it. This keeps the tag pane unambiguous: a human can filter `engram/*` to see all agent flags at once, or hide them entirely.

The Cartographer's quarterly audit normalizes user tags (synonym detection, hierarchy gaps) but never touches `engram/*`.

---

## Wikilinks

Standard Obsidian:

```markdown
[[Attention as lossy compression]]                      # by title
[[Attention as lossy compression|attention]]            # with display alias
[[Attention as lossy compression#^claim-compression]]   # by block ID
[[01JRZK3M7P|attention]]                                # by ID (when title is ambiguous)
```

Agents prefer the title form for human readability. When titles are ambiguous (rare, given collision handling), they fall back to the ID form, which works because the ID is also kept in `aliases:` automatically.

External links use standard markdown: `[text](https://...)`.

The link graph in the index stores both source-by-id and source-by-title; both forms resolve correctly.

---

## Block IDs

Obsidian's `^block-id` syntax. Agents add a block ID when they want to reference a specific claim later (e.g., for a deliberation or a counter-argument):

```markdown
Attention mechanisms perform lossy compression of context into a fixed-size representation. ^claim-attention-compression

This connects to [[Rate-distortion theory]].
```

Block ID conventions:

- Kebab-case
- Prefix with the kind: `claim-`, `def-`, `example-`, `ref-`
- Stable: once a block ID exists, agents don't change it (humans may)
- Agent-added block IDs are listed in sidecar (`block_ids: ["claim-attention-compression"]`) for fast lookup

Now Devil's Advocate can challenge `[[Attention as lossy compression#^claim-attention-compression]]` precisely. Humans can ignore block IDs (Obsidian renders cleanly).

---

## Provenance markers

Block-level attribution via HTML comments. Invisible in Obsidian's rendered view, visible in raw markdown and `git diff`.

```markdown
This connects to [[Rate-distortion theory]] in information theory.
<!-- by: linker confidence: 0.93 -->

Attention mechanisms perform lossy compression. ^claim-attention-compression
<!-- by: synthesizer deliberation: 2026-04-15-0003 -->
```

Format:

```
<!-- by: <agent-name> [confidence: <0.0-1.0>] [deliberation: <id>] -->
```

Rules:

- Agents add a comment after every block they author or modify.
- The comment is part of the block --- if the user moves the paragraph, the attribution moves with it.
- Comments are short. Long rationale lives in the sidecar's `provenance_history`.
- Humans don't write provenance comments; their work is the absence of any comment.
- A block with no provenance comment is presumed human-authored.

When `git restore` discards an agent change, the comment goes with it. The `agent_actions` row preserves the attempted attribution regardless.

---

## Obsidian plugin compatibility

These conventions are designed to coexist with the Obsidian plugin ecosystem, not fight it.

| Plugin / feature      | Status      | Notes                                                                                  |
| --------------------- | ----------- | -------------------------------------------------------------------------------------- |
| Dataview              | Works       | Frontmatter is structured and queryable. Reserved `engram/*` tags are filterable.      |
| Templater             | Works       | No conflict. Users can templer-prefix new notes with their preferred frontmatter.       |
| Daily Notes           | Works       | Engram's `journal/` follows whatever daily-notes path/format the user has configured.   |
| Graph view            | Works       | Standard wikilinks. Filenames are clean.                                                |
| Tag pane              | Works       | `engram/*` is filterable; user tags are normal.                                        |
| Search                | Works       | Standard markdown content. Engram's hybrid search is supplementary, not a replacement. |
| Backlinks pane        | Works       | Wikilinks are standard.                                                                 |
| Front Matter Title    | Not needed  | Filenames are already clean; no plugin required to make Obsidian show the right name.   |
| Templater + agents    | Compatible  | Templater-created notes get an `id` assigned by engram on the next file-watcher pass.   |

Engram is a **layer over** an Obsidian vault, not a replacement for it. Anything the user does natively in Obsidian works.

---

## Quick reference

| Concern                   | Where it lives                                |
| ------------------------- | --------------------------------------------- |
| Display title             | filename slug + `title:` frontmatter          |
| Canonical reference       | `id:` frontmatter (ULID)                      |
| Note classification       | `type:`, `status:` frontmatter                |
| Tags (user)               | `tags:` frontmatter, `topic/*`, `area/*`, etc. |
| Tags (agent flags)        | `tags:` frontmatter, `engram/*`               |
| Citations                 | `## Sources` section                          |
| Wikilinks                 | inline + `## Connections` section             |
| Provenance (per block)    | HTML comments inline                          |
| Provenance (full history) | `.engram/sidecar/<id>.json`                   |
| Embedding metadata        | sidecar                                       |
| Agent visit log           | sidecar                                       |
| Block-precise references  | `^block-id` syntax                            |
| Agent action audit        | `agent_actions` table (sqlite)                |
| Council transcripts       | `.engram/deliberations/`                      |
| Pending agent changes     | `git status` (unstaged working tree)          |
