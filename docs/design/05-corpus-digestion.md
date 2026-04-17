# Corpus Digestion

## Purpose

Ingestion (`02-ingestion.md`) handles individual files dropped into engram. **Corpus digestion is different.** It handles whole external note corpora --- typically a previous Obsidian vault that has grown unwieldy --- and produces a curated, smaller, evergreen-quality body of content in engram.

The motivating case: an existing vault at `notes-2022-03/` containing thousands of notes accumulated over years. Most of it is fleeting captures, half-formed ideas, meeting notes, and quotes that were useful at the time but no longer earn their space. A small fraction contains genuinely valuable thinking that should become evergreen content in engram. The rest should be summarized, archived, or discarded.

The cardinal rule: **the source corpus is read-only and never modified.** Engram reads, analyzes, distills. The user decides whether to delete the original later --- engram never does.

The other cardinal rule: **the Curator must be willing to discard.** A digestion that keeps everything has failed. The point is to produce a leaner, higher-quality vault than the source.

---

## The Curator agent

A specialized agent dedicated to corpus digestion. Distinct from Ingestor (single-file) and from the rest of the swarm. Curator is invoked explicitly by the user, runs over weeks for large corpora, and orchestrates several other agents (Synthesizer, Merger, Linker, Scribe) as sub-tools.

- **Job:** Process an external note corpus into engram's vault, deciding per-note whether to keep, distill, merge, archive, or discard.
- **Trigger:** `engram digest <path>` or via Swift app.
- **Tools:** Hybrid retrieval (across both source corpus and existing engram vault), embedding clustering, evergreen rubric checker, sub-agent invocation (Synthesizer, Merger, Linker, Scribe).
- **Model tier:** `standard` for individual disposition decisions; `deep` for cluster-level synthesis.
- **State:** Persistent. Digestion can pause and resume across days/weeks.

---

## Pipeline

```
User: engram digest /path/to/old-vault
    |
    v
1. SURVEY (Curator, read-only on source)
   - Walk the source corpus. Build a structural map:
     - Note count, size distribution, age distribution
     - Tag taxonomy (with counts)
     - Link graph (internal density, dangling links)
     - Modification cadence
   - Embed every note. Cluster by topic.
   - Per-note initial classification proposal:
     - "looks like an evergreen-ready concept note"
     - "looks like quoted/source material"
     - "looks like meeting notes / fleeting captures"
     - "looks like a draft that was never finished"
     - "looks redundant with existing engram content"
     - "no apparent value"
   - Output: digestion plan in .engram/digestion/<source-slug>/plan.md
    |
    v
2. PLAN REVIEW (human)
   - User reads the plan. Adjusts heuristics:
     - "Don't auto-discard anything tagged #important"
     - "Treat all notes in /journal/ as personal (private, route to Witness)"
     - "Cluster around 'travel' aggressively --- merge anything related"
     - "Reject any synthesis that loses the original wording for #poetry notes"
   - Plan becomes authoritative. Curator records the policy.
    |
    v
3. BATCH DIGESTION (Curator, with sub-agents)
   - Process in chunks of 50 notes (configurable).
   - For each note in batch, decide a disposition:

     | Disposition          | Action                                                          |
     |----------------------|-----------------------------------------------------------------|
     | keep-evergreen-draft | Synthesizer drafts an evergreen note from the source content    |
     | keep-literature      | Scribe formats as a literature note; original stays read-only   |
     | merge-into:<note-id> | Append insights to an existing engram note (Merger logic)       |
     | archive              | Copy verbatim to notes/archive/, type: archive (low agent activity) |
     | discard              | Not imported. Logged with one-line summary in case of regret.   |
     | defer                | Not sure yet. Re-evaluate in a future batch.                    |

   - Per batch: produces a batch report (~50 dispositions with rationale and drafts).
    |
    v
4. BATCH REVIEW (human, in Swift app)
   - Review each batch as a single unit (not 50 separate prompts).
   - Per-item options: approve, override (change disposition), edit-draft, reject.
   - Bulk operations: "approve all literature", "discard all in this cluster".
   - Outputs: approved drafts move toward landing; overrides update Curator's policy.
    |
    v
5. INTEGRATION (engram's normal flows)
   - Approved drafts enter the standard council pipeline.
   - Linker proposes connections to existing engram content.
   - Steelman / Devil's Advocate may engage on evergreen drafts.
   - Some drafts will fail downstream review and be shelved or revised.
    |
    v
6. AUDIT (Auditor, after digestion completes)
   - Random-sample the discards. Did Curator throw away anything valuable?
   - Random-sample the kept-as-archive items. Should any be promoted?
   - Cross-check against the user's stated policy adjustments.
   - Output: digestion audit note. If significant misses, recommend re-running specific clusters.
```

---

## Disposition definitions

Each external note ends up in exactly one of six states.

### `keep-evergreen-draft`

The source note expresses a concept atomically and is worth preserving as engram content. Synthesizer drafts an evergreen note in engram's voice (subject to Voice Keeper). The new note's frontmatter records the source:

```yaml
---
id: 01JRZ...
title: <derived title>
type: evergreen
status: candidate-evergreen
source_corpus: notes-2022-03
---

<!-- Sidecar at .engram/sidecar/01JRZ.json holds:
     ingestion: { via: "curator", at, source_path, source_hash, ... }
     and provenance history. See 06-note-conventions.md. -->
```

Goes through the standard evergreen birth ceremony before earning `status: evergreen`.

### `keep-literature`

The source note is a record of external material (quotes, summaries, reading notes). Scribe formats as a literature note with the source preserved as a referenced artifact. The original file's content becomes the artifact (stored in `.engram/artifacts/`, content-addressed).

### `merge-into:<note-id>`

The source covers ground that already exists in engram. Merger appends the new insights into the existing note (with provenance markers showing what came from the digestion).

### `archive`

Worth preserving the original verbatim, but not worth processing into evergreen form. Examples: meeting notes, dated journal entries with sentimental value, raw transcripts. Stored in `notes/archive/<source-slug>/<original-path>` with `type: archive` frontmatter. Most agents skip notes of type `archive` (only Cartographer indexes them; nothing rewrites them). They're searchable but don't participate in the active vault economy.

### `discard`

Not worth keeping. Not imported. Logged with a one-line summary in `.engram/digestion/<source-slug>/discards.md` so the user can sanity-check what was thrown away. Discards never delete the source --- the source corpus is read-only. The user retains the option to delete the source later or never.

### `defer`

Curator can't confidently classify. Re-evaluated in a future batch (typically after similar notes have been processed and the policy is clearer). Persists across sessions.

---

## Curation policy

The Curator's behavior is governed by a policy file: `.engram/digestion/<source-slug>/policy.toml`. Initial values are heuristics; the user adjusts based on the survey output and the first few batches.

```toml
[policy]
# How aggressive should discards be?
# strict = throw away anything not clearly evergreen-worthy
# balanced = keep things that might be useful later
# lenient = err on the side of keeping
discard_aggressiveness = "balanced"

# Minimum length (characters) to consider for evergreen draft
min_evergreen_length = 200

# Minimum link density (outgoing wikilinks / 100 words) to consider evergreen
min_evergreen_link_density = 0.5

# Cluster size threshold for triggering Synthesizer (instead of per-note processing)
cluster_synthesis_threshold = 5

[[overrides]]
# Never auto-discard certain tags
match = { tag = "important" }
disposition = "keep-or-archive"   # forces a non-discard outcome

[[overrides]]
match = { path_prefix = "journal/" }
disposition = "archive"
private = true                    # also route to Witness, never cloud LLM

[[overrides]]
match = { tag = "meeting" }
disposition = "discard"           # meeting notes typically not worth keeping
keep_summary = true               # but summarize for the discard log

[batching]
batch_size = 50
parallel_batches = 1              # serial review is more thoughtful

[clustering]
# When N+ notes are about the same topic, propose a single synthesized evergreen
# rather than N separate evergreen drafts
synthesis_threshold = 5
```

---

## Resumability and state

Large corpora (10K+ notes) are not digested in one session. Curator state is persisted in SQLite:

```sql
CREATE TABLE corpus_digestions (
    id              TEXT PRIMARY KEY,         -- ULID
    source_path     TEXT NOT NULL,            -- absolute path to source corpus
    source_slug     TEXT NOT NULL,            -- short name, used for paths
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    status          TEXT NOT NULL,            -- surveying, planned, digesting, completed, paused
    total_notes     INTEGER,                  -- count after survey
    notes_processed INTEGER NOT NULL DEFAULT 0,
    notes_kept      INTEGER NOT NULL DEFAULT 0,
    notes_discarded INTEGER NOT NULL DEFAULT 0,
    notes_archived  INTEGER NOT NULL DEFAULT 0,
    notes_merged    INTEGER NOT NULL DEFAULT 0,
    policy_path     TEXT
);

CREATE TABLE digestion_items (
    id              TEXT PRIMARY KEY,         -- ULID
    digestion_id    TEXT NOT NULL REFERENCES corpus_digestions(id),
    source_path     TEXT NOT NULL,            -- relative to source corpus root
    source_hash     TEXT NOT NULL,            -- SHA-256 of source content
    cluster_id      TEXT,                     -- assigned during survey
    initial_class   TEXT,                     -- Curator's initial classification
    disposition     TEXT,                     -- final disposition (NULL until decided)
    engram_note_id  TEXT REFERENCES notes(id), -- if kept, the resulting engram note
    batch_id        TEXT,                     -- which batch processed it
    status          TEXT NOT NULL DEFAULT 'pending', -- pending, drafted, approved, rejected, deferred
    decided_at      TEXT,
    rationale       TEXT                      -- why this disposition
);
CREATE INDEX idx_digestion_items_digestion ON digestion_items(digestion_id);
CREATE INDEX idx_digestion_items_status ON digestion_items(digestion_id, status);

CREATE TABLE digestion_clusters (
    id              TEXT PRIMARY KEY,
    digestion_id    TEXT NOT NULL REFERENCES corpus_digestions(id),
    centroid_topic  TEXT,                     -- LLM-generated cluster name
    note_count      INTEGER NOT NULL,
    proposed_action TEXT,                     -- synthesize | individual | discard-cluster
    synthesis_note_id TEXT REFERENCES notes(id) -- if synthesized into one note
);

CREATE TABLE digestion_discards (
    digestion_item_id TEXT NOT NULL REFERENCES digestion_items(id),
    summary           TEXT NOT NULL,          -- one-line summary preserved in case of regret
    discarded_at      TEXT NOT NULL,
    PRIMARY KEY (digestion_item_id)
);
```

This means digestion can pause and resume cleanly. Curator records progress incrementally; killing the process and restarting picks up where it left off.

---

## CLI

```bash
# Start a digestion
engram digest /path/to/old-vault

# Resume an in-progress digestion
engram digest --resume <digestion-id>

# Status of all digestions
engram digest --list

# Show the survey plan for a digestion
engram digest --plan <digestion-id>

# Adjust policy mid-digestion
engram digest --policy <digestion-id> --edit

# Re-run a specific cluster with new policy
engram digest --recluster <digestion-id> <cluster-id>

# Show the discard log
engram digest --discards <digestion-id>

# Audit a completed digestion
engram digest --audit <digestion-id>
```

---

## Cluster-level synthesis

A key reason corpus-level processing is more powerful than per-file ingestion: **clusters can be synthesized.** When the survey identifies (e.g.) 12 notes about "attention mechanisms" written between 2023 and 2026, three options exist:

1. **Synthesize one evergreen** that names the underlying concept and absorbs the best content from all 12. Most aggressive; requires Synthesizer + human approval.
2. **Process individually** with `merge-into` chains so the strongest one becomes the canonical and others contribute fragments.
3. **Discard the cluster as redundant** if engram's existing vault already covers the same ground better.

Curator proposes option (1) when cluster size exceeds `cluster_synthesis_threshold` and the cluster is internally coherent (high pairwise similarity). Otherwise (2). Discards happen at cluster level when the user explicitly authorizes via policy override.

The synthesized evergreen draft cites all 12 source notes in its frontmatter:

```yaml
---
id: 01JRZ...
title: Attention as soft dictionary lookup
type: evergreen
status: candidate-evergreen
source_corpus: notes-2022-03
---

<!-- Sidecar at .engram/sidecar/01JRZ.json holds the full source list,
     synthesized_from_cluster ID, and ingestion metadata:
     {
       "ingestion": { "via": "curator", "synthesized_from_cluster": "01JRZ..." },
       "sources": [
         { "corpus": "notes-2022-03", "path": "ML/attention-1.md", "hash": "..." },
         { "corpus": "notes-2022-03", "path": "ML/attention-2.md", "hash": "..." }
         // ... all 12
       ]
     }
-->
```

The cluster's discard log records: "Cluster of 12 notes about attention. Synthesized into [[01JRZ...]]. Originals not imported."

---

## Privacy and routing

The standard privacy model applies. Source notes matching a privacy zone in the user's policy are routed to local-only processing (no cloud LLM). Personal/journal-style content (`tag: personal`, paths under `journal/`, etc.) defaults to Witness routing if the user has Witness enabled.

External MCP clients (`04-external-mcp.md`) **never see the source corpus.** Digestion is a local-only flow until results land in the engram vault as approved notes.

---

## Worked example: the notes-2022-03 vault

Concrete walkthrough of the user's actual situation.

### Survey output (hypothetical)

```
Source: /Users/torsday/src/github.com/torsday/notes-2022-03
Total notes: 9,247
Tag count: 412
Top tags: type/evergreen (1,053), tech/llm (487), meeting (391), ...
Avg note size: 2.3 KB
Modification span: 2022-03 to 2026-04

Initial classification (heuristic):
  evergreen-ready candidates:    1,053 (notes already tagged type/evergreen)
  literature-style:              2,841 (quotes, summaries, reading notes)
  meeting-notes / fleeting:      3,012 (likely discard-after-summary)
  drafts (status: in-progress):    487 (defer; review individually)
  redundant (cluster duplicates): 1,243 (cluster-level decisions)
  other:                           611

Top concept clusters (>5 notes):
  attention/transformers (47 notes)
  Zettelkasten/notes-on-notes (38 notes)
  observability/logging (29 notes)
  agents/agentic-ai (62 notes)
  ... 80 more clusters

Recommended initial policy:
  discard_aggressiveness: balanced
  cluster_synthesis_threshold: 8 (this corpus has many natural clusters)
  override: tag=meeting -> discard with summary
  override: path_prefix=Journal/ -> archive (private)
  override: tag=evergreen -> evaluate individually (don't auto-discard)
```

### Plan review

The user adjusts the policy: keeps balanced aggressiveness, raises cluster synthesis threshold to 10 (less aggressive synthesis on this corpus), adds an override for `tag=poetry` to never discard.

### First batch

Curator processes 50 notes. Output:

```
Batch 001: 50 notes processed
  evergreen-draft:    8 (synthesizer drafts attached)
  literature:         18 (formatted by scribe)
  merge-into:         3  (proposed merges with existing engram notes)
  archive:            6  (meeting notes + dated journal entries)
  discard:            12 (with one-line summaries)
  defer:              3
```

User reviews in Swift app. Approves 36, overrides 5 (e.g., promotes one "discard" to "archive"), edits 6 drafts, rejects 3. Curator updates its policy weights based on the overrides.

### Over weeks

Digestion continues at the user's pace. Pacekeeper throttles the producing agents if the review backlog grows. After 4--6 weeks, the corpus is fully digested. Final state:

```
Source: 9,247 notes
Result:
  evergreen drafts (now landed): ~600
  literature notes:              ~1,400
  archive (preserved verbatim):  ~1,800
  merged into existing notes:    ~140
  discarded (not imported):      ~5,300
Compression: 9,247 → ~2,000 active engram notes (78% reduction)
```

The user's engram vault now contains ~2K curated, evergreen-quality notes drawn from a 9K source corpus, plus an archive of source-of-truth-but-not-active material. The original `notes-2022-03/` is unchanged --- the user can keep it indefinitely or delete it after Auditor's review confirms nothing valuable was lost.

---

## Open questions

- **Re-digestion.** If the user updates the policy and wants to re-run on a subset, the system should support this without re-doing already-approved work. Implemented as `engram digest --recluster`.
- **Cross-corpus deduplication.** If the user digests two different external corpora that overlap, Merger should detect this and propose unification. Should work via existing Merger agent.
- **Incremental digestion.** If the source corpus is still being added to (the user is still writing in `notes-2022-03/` while digesting), Curator should detect new/modified source notes and queue them for the next batch. v2.
- **Digestion of engram itself.** Can engram digest *itself* to compress over time? Probably yes, but with extra caution --- the source isn't read-only in this case. Defer until the basic flow is solid.
