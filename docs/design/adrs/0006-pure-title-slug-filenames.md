# ADR 0006: Filenames are pure title-slugs; ID lives in frontmatter only

**Status:** Accepted

**Date:** 2026-04 (during note-conventions design)

## Context

Every note has a stable ULID `id:` for canonical reference (so renames don't break agent references). The question is where the ID appears: in the filename, or only in the frontmatter.

ULID-prefixed filenames (`01JRZK3M-attention-as-lossy-compression.md`) are the standard Zettelkasten convention. They're rename-stable, sortable by creation time, and unambiguously unique. But they're also **visible everywhere in Obsidian**: the file sidebar, the quick switcher results, the graph view node labels, the backlinks pane. Every interaction with the note shows the ID prefix.

Pure title-slug filenames (`attention-as-lossy-compression.md`) are clean for humans but introduce duplicate-name handling and require an out-of-filename mechanism for stable references.

## Decision

**Filenames are pure title-slugs.** No IDs, hashes, or dates in filenames (except for inherently-temporal types like `journal/2026-04-17.md`).

The `id:` ULID lives in frontmatter only. It is never visible in Obsidian's file sidebar, quick switcher, graph view, or backlinks pane.

Slug collisions are handled at write time by Cartographer, which appends the smallest disambiguator that resolves the conflict (`-2`, `-3`). Most notes never collide. Existing notes are never renamed to add `-1`.

Renames are tracked via the surviving ID across paths: when file at path A (with ID X) disappears and file at path B (with ID X) appears, the file watcher recognizes a rename, not a delete-plus-create. The link graph in sqlite is ID-based; wikilinks resolve by title with the ID as fallback.

## Alternatives considered

1. **ULID-only filename** (`01JRZK3M7P.md`). Maximum stability; minimum readability. Common in Zettelkasten but punishing in Obsidian's UI.
2. **ULID prefix + slug** (`01JRZK3M-attention-as-lossy-compression.md`). The Andy-Matuschak-style compromise. Stability + readability. Cost: the ID is still visible everywhere.
3. **Title-only filename + ID in frontmatter, no collision detection.** Risky: silent collisions overwrite or corrupt.
4. **Title-only filename + ID in frontmatter + collision detection at write.** Chosen.

## Decision rationale

- The Swift app and Obsidian sidebar are the user's most-frequent interfaces with files. ID prefixes pollute both.
- Renaming via Obsidian's native rename works correctly because IDs survive in frontmatter and the file watcher reconciles.
- Most notes never collide (the slug space is huge for typical knowledge content). Cartographer handles the rare cases.
- The "Front Matter Title" Obsidian plugin can render alternative names, but relying on a plugin to make the basic experience tolerable is wrong --- and it doesn't help in graph view or other plugin-bypass surfaces.

## Consequences

**Positive:**

- **Clean Obsidian experience throughout.** Sidebar, quick switcher, graph, backlinks --- all show readable titles.
- **Wikilinks remain natural** (`[[Attention as lossy compression]]`).
- **Native Obsidian renames work** (Obsidian updates wikilinks; engram tracks via ID).
- **No plugin dependencies** for usable filenames.

**Negative:**

- **Manual file creation in Obsidian creates a note without an ID.** Mitigation: the file watcher detects new files lacking `id:` frontmatter and proposes adding one (an unstaged `id:` insertion the user reviews and stages).
- **Slug collisions require coordination.** Mitigation: Cartographer detects and proposes resolution at write time; manual collisions surface as warnings on next index pass.
- **Renames must be detected via ID survival, not filename.** Mitigation: the file watcher does this; the link graph is ID-keyed; this is correct behavior, not a workaround.
- **Cartographer becomes responsible for naming hygiene.** Mitigation: this is already Cartographer's job (MOCs, indices, tag taxonomy); naming is a natural extension.

## References

- `06-note-conventions.md` --- filename rules, slugging, collision handling, rename tracking
- `01-agents-and-council.md` --- "Stable note IDs" section
- ADR 0005 --- sidecar JSON (the same dual-citizen principle, applied to metadata)
