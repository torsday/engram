# engram: Overview

> Your thoughts, encoded. A living knowledge base that rewrites itself.

## Vision

engram is a knowledge system where autonomous agents continuously improve a personal vault of notes. The vault is plain markdown, versioned with git, browsed in Obsidian. Agents create, link, restructure, challenge, and prune notes through a deliberation process that keeps the human in control of consequential changes while letting mechanical work happen silently.

The north star is the **evergreen note**: atomic, concept-oriented, densely linked, written for a future self. Every agent action is measured against whether it moves the vault closer to that ideal.

## What engram is not

- **Not another "chat with your vault."** Chat optimizes for fast answers from a static corpus. engram optimizes for generating new thinking occasions. The system surfaces questions, contradictions, and connections --- it does not produce answers on your behalf.
- **Not an oracle.** Agents are epistemic rubber ducks, not authorities. They propose, critique, and ask. The human writes, decides, and approves.
- **Not a replacement for writing.** Writing is thinking. engram guards this by separating what agents may do (link, restructure, question) from what they may not (replace the human's voice in evergreen notes). Mechanical cleanup is automated; intellectual labor is surfaced, never bypassed.

## Principles

1. **The vault is canonical.** Obsidian markdown + git is the source of truth. Every index, embedding, and cache is derived and rebuildable. Delete `.engram/index.sqlite` and the system reconstructs.
2. **Provenance is universal.** Every byte in the vault knows who wrote it (human or which agent), when, and under what deliberation. Block-level attribution uses hidden HTML comments; the agent-actions log records every unstaged write. After a year, you can still tell your thoughts from the swarm's.
3. **Agents are data, not code.** Each agent is a directory: a prompt, a config, optional tool declarations. Adding an agent means copying a directory. No recompile, no deploy. The swarm stays legible.
4. **Deliberation is typed, not conversational.** Agent coordination follows a state machine (`DRAFT -> CRITIQUE -> REVISE -> CONVERGE -> {LAND | PROPOSE | SHELVE}`), not a free-form chat. Bounded rounds. Schemas enforced by types. No infinite loops.
5. **Evergreen is the north star.** The evergreen note rubric (atomic, concept-oriented, densely linked, claim-titled, non-redundant) is the shared contract agents argue under. Every proposed change must answer: does this move the vault toward more evergreen, or less?
6. **Confidence-gated, git-safe autonomy.** Agents self-assess confidence on every action. Below threshold: explicit proposal through the council. Above threshold: the agent writes to the working tree --- but never stages or commits. **Only the human runs `git add` and `git commit`.** Every agent action is reviewable via `git diff` and revertible via `git restore`. The unstaged diff *is* the proposal queue. This is a two-layer safety net: confidence gates what reaches the working tree; git gates what reaches history.
7. **Graduated autonomy.** Trust scores modulate confidence thresholds: high-trust agents may act on lower confidence; low-trust agents must clear higher bars. The system earns trust incrementally based on outcome metrics, not just acceptance.
8. **Disagreement is a first-class output.** When agents can't agree, the draft is shelved with the dissent annotated. Contested proposals become prompts for the human to decide --- and the disagreement itself is a useful artifact.

## Surfaces

engram is one system with multiple views over the same vault:

- **Obsidian** --- the primary reading and writing environment. The vault lives here. Agents' work appears as new/modified files.
- **Swift app (iOS + macOS)** --- four roles: **capture** (voice, text, photos, share-sheet, file drop --- with a local offline queue so capture works even when the Mac is unreachable, syncing on reconnect), **diff review** (review unstaged agent changes per-file, approve via stage or reject via restore, on the go), **search** (hybrid query against the vault when away from Obsidian), and **browse** (read the vault without Obsidian open). Capture is the top priority --- the failure mode of a knowledge tool is friction at the entry point.
- **Claude via MCP** --- the vault exposed as tools (`search_notes`, `read_note`, `follow_backlinks`, etc.) so Claude Desktop/Code can reason over it directly.
- **CLI** --- `engram serve`, `engram reindex`, `engram run <agent>`, `engram status`. Developer-facing, automation-friendly.
- **Web review UI** --- deferred. If the Swift app handles review well, this may never ship.

## Note type hierarchy

Not all notes are equal. The type determines what agents may do with it:

| Type             | Description                                                                                                                            | Agent permissions                                                                                                                |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| **Fleeting**     | Quick captures from the Swift app. Stream of consciousness, voice transcripts, share-sheet dumps.                                      | Scribe may rewrite freely. Ingestor may extract and restructure. Linker may propose connections.                                 |
| **Literature**   | One per ingested source. Links to the raw artifact. Contains summary, extracted text, citation. Source-oriented, not concept-oriented. | Scribe formats. Linker connects to existing notes. Synthesizer may propose derived evergreen notes.                              |
| **Evergreen**    | The curated core. Atomic, concept-oriented, densely linked. Written for the future self.                                               | Agents may link, tag, and question. Rewrites require council deliberation + human approval. The human's voice is protected here. |
| **MOC / Index**  | Maps of content, tag indices, `index.md`. Navigation, not substance.                                                                   | Cartographer owns these. Auto-updated.                                                                                           |
| **Archive**      | Verbatim-preserved content from corpus digestion (see `05-corpus-digestion.md`) that the user wanted to keep but not actively curate.  | Read-only. Most agents skip these. Only Cartographer indexes them. Searchable but inert.                                         |
| **Deliberation** | Council transcripts stored in `.engram/deliberations/`. The reasoning trail behind vault changes.                                      | Read-only after creation. Referenced by provenance metadata.                                                                     |

Frontmatter `type:` field controls permissions. An untyped note defaults to fleeting.

The full filename, frontmatter, sidecar, folder, tag, wikilink, and provenance conventions live in [`06-note-conventions.md`](06-note-conventions.md). The short version: filenames are pure title-slugs (no IDs visible to humans), frontmatter stays lean (Obsidian-friendly), and rich agent metadata lives in per-note sidecar JSON at `.engram/sidecar/<id>.json` (git-tracked, diff-readable). The two layers are tied together by the ULID `id:` in frontmatter.

## The evergreen rubric

A note earns `status: evergreen` when it passes:

- **Atomic** --- expresses one idea. If it can be cleanly split, it should be.
- **Concept-titled** --- title is a concept name or a claim, not "Notes from X" or a date.
- **Densely linked** --- meaningful outgoing wikilinks to related concepts. Fewer than two is a smell.
- **Non-redundant** --- no existing note already covers this idea. If one does, merge or differentiate.
- **Source-independent** --- stands on its own. Doesn't require reading the source to make sense.
- **Revisitable** --- free of unresolved TODOs, hedging ("I think maybe..."), and temporal references ("yesterday").

Agents check this rubric programmatically. The Socratic Prober stress-tests notes before they earn the evergreen label.

## Review and agency model

Three mechanisms, used at different altitudes. **All three converge on the same final gate: an unstaged `git diff` the human reviews before staging and committing.**

1. **Confident autonomous write.** When an agent's self-assessed confidence on a low-invasiveness change clears its threshold, it writes directly to the working tree. The change is unstaged. Logged in the agent-actions table with confidence score and rationale. The human reviews via `git diff` and either stages it (acceptance) or runs `git restore` (rejection). No council overhead for changes the agent is sure about and the user can verify in seconds.
2. **Council deliberation.** Semantic changes the proposing agent isn't sure about, or that exceed its invasiveness ceiling. Multiple agents discuss in bounded rounds. If consensus, the change is written to the working tree (still unstaged). If dissent, shelved.
3. **Explicit human approval.** Consequential changes: new evergreen notes, section rewrites, note merges, deletions. Council produces a proposal that enters the Swift-app review queue. Human approves; the change is then written to the working tree, unstaged, just like the others.

Every path ends with **the human as the only entity that runs `git add` and `git commit`.** This means:

- **No agent ever modifies git history.** Period.
- **`git status` is always the source of truth for "what's pending."**
- **`git restore <path>` is always a valid undo.**
- **Provenance survives revert.** Block-level HTML comments and the agent-actions table preserve the record of what the agent did even if the user discards the change.

The Swift app surfaces unstaged-diff review as its primary review interface --- per-file diff with agent attribution, approve (stage) or reject (`git restore`) inline.

## Open questions

- **Multi-device sync.** If the Swift app is used from iOS away from the Mac, the vault needs to be reachable. v1: same-network or Tailscale. Sync is a v2 problem.
- **Privacy routing.** Some artifacts (work documents, medical records) should never hit cloud LLMs. Per-drop and per-folder privacy flags route to local-only extraction. Exact UX TBD.
- **Scale ceiling.** The architecture is sized for ~10K notes. At 50K+, sqlite-vec and FTS5 may need rethinking. Defer until it's a real problem.
- **Agent tuning feedback loop.** The Watcher agent monitors rejection rates and proposes config changes. Whether this is sufficient or whether a more formal RLHF-like loop is needed is an open question.
