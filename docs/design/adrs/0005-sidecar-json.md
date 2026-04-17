# ADR 0005: Rich agent metadata in sidecar JSON, not extended frontmatter

**Status:** Accepted

**Date:** 2026-04 (during note-conventions design)

## Context

Agents need to track a lot per-note: provenance history (every event this note has experienced), embedding metadata (model, version, hash), agent visit log, rubric check history, calibration claims, deliberation pointers, ingestion metadata (source artifact hash, ingestor identity, source path for digested content). The naive home for all this is the note's YAML frontmatter.

But: the note must also be human-readable in Obsidian. A note opening with 30+ lines of agent-tracking YAML is hostile to the reader. The two needs --- machine-rich metadata, human-clean reading surface --- conflict if everything lives in one place.

## Decision

**Two layers, one identity.**

- The markdown file holds **lean human-relevant frontmatter** (5--7 lines: `id`, `title`, `type`, `status`, `created`, `tags`, `aliases`).
- A **sidecar JSON file** at `.engram/sidecar/<id>.json` holds everything else: provenance history, embedding metadata, agent visit log, rubric check history, calibration claims, ingestion details.
- The two are tied together by the ULID `id:` in frontmatter.
- **Sidecars are git-tracked.** They're durable record, not derived state.
- Sidecars are pretty-printed JSON, one field per line, diff-readable.

## Alternatives considered

1. **Extended frontmatter** (everything in YAML at the top of the note). Rejected: hostile to humans reading in Obsidian, even with frontmatter folded.
2. **Hidden in-file YAML block** (a second YAML block under a fold marker). Rejected: still pollutes the file, doesn't fold cleanly in all Obsidian plugins.
3. **SQLite-only** (no sidecar file; rich metadata lives in the index). Rejected: not portable across vault clones; loses durable record on `engram reindex`.
4. **Sidecar in a different format** (msgpack, CBOR, sqlite blob). Smaller, but loses git-diff readability. Rejected.
5. **Sidecar pretty JSON, git-tracked.** Chosen.

## Consequences

**Positive:**

- **Humans see a clean note.** Frontmatter folds away cleanly in Obsidian; reading the note is uncluttered.
- **Agents have rich, structured context.** Sidecar JSON is purpose-built for agent consumption.
- **Sidecars travel with the vault.** Clone the vault and the per-note history comes along.
- **Diff-readable.** Agent edits to sidecars show up in `git diff` per ADR 0003 --- the human can review what an agent changed in metadata, not just markdown.
- **Portability.** Anyone can read a sidecar (it's JSON) without engram. The vault remains "just a directory of markdown plus a pile of JSON" --- no vendor lock-in.

**Negative:**

- **Two files per note.** Slightly more cognitive overhead during file operations. Mitigation: the file watcher and indexer treat sidecar updates as part of the note's lifecycle; they're never drifted out of sync.
- **Sidecar growth over time.** A note that accumulates many agent visits will grow its sidecar. Mitigation: Gardener prunes `agent_visit_log` (last 100 per agent) and `rubric_check_history` (last 50) quarterly. Provenance history is never pruned --- it's durable record.
- **Sidecar deletion semantics.** When a note is deleted, its sidecar should also be deleted. The git-ops layer handles this atomically.
- **Schema migration required.** When the sidecar schema evolves, every existing sidecar needs upgrading. Mitigation: each sidecar carries `schema_version`; the loader applies in-memory upgrades and rewrites at the current version.

## References

- `06-note-conventions.md` --- the layering principle and the full sidecar schema
- `03-architecture.md` --- `.engram/sidecar/` directory entry
- ADR 0006 --- pure title-slug filenames (the same dual-citizen principle, applied to filenames)
