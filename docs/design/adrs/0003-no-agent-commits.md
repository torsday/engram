# ADR 0003: Agents never run `git add` or `git commit`

**Status:** Accepted

**Date:** 2026-04 (during initial design); reaffirmed when confidence-gated autonomy was added

## Context

Engram's agents modify the vault: they add wikilinks, fix dead links, propose new notes, restructure content. The natural question is how those changes interact with git.

The original design had agents committing autonomously: each agent has a git identity (`linker <linker@engram.local>`), and accepted changes produced commits authored by the agent. The audit trail was `git log --author=<agent-name>`. Reverts were standard `git revert`.

After designing confidence-gated autonomy, this model felt wrong. The problem: even at high confidence, an agent's change should be one the user has reviewed and accepted. Auto-commits to the working tree mean the user's review surface is `git log` (after the fact) rather than `git status` (before the fact). The user's only revert path is destructive (`git revert` introduces a new commit that undoes the prior one --- the original is still in history).

## Decision

**Agents never run `git add` or `git commit`.** All agent writes land in the working tree as **unstaged** changes. The user is the only entity that runs `git add` or `git commit`. The unstaged `git diff` is the universal review surface. `git restore <path>` is the universal undo.

The git layer in `engram-core` exposes a read-only handle to the agent runtime; only the API/CLI surfaces invoked by the user can issue write-side git operations. This is enforced at the type-system level: agent code does not receive a writable git handle.

## Alternatives considered

1. **Agent commits, human reviews via `git log`.** The original design. Rejected because review-after-commit is worse UX than review-before-commit, and reverts are destructive.
2. **Agent branches + PRs.** Each agent works on its own branch; council-approved changes open PRs for the human to merge. Rejected as too much git ceremony for everyday operations and overkill for low-invasiveness changes.
3. **Agent commits to a staging branch, human merges to main.** Better than per-agent branches but still adds a merge step for every change. Rejected.
4. **Agents only write unstaged.** Chosen.

## Consequences

**Positive:**

- **`git status` is the source of truth for "what's pending."** Trivial mental model. Trivial Swift-app surface.
- **`git restore <path>` is always a valid undo.** No git history pollution from rejected agent work.
- **The user is the only entity touching history.** History stays clean. Commit messages reflect human intent.
- **Aggression-without-risk.** Agents can be aggressive within their working-tree sandbox precisely because nothing they do reaches history without the human's `git add`.
- **Pre-commit hooks remain meaningful.** When the user commits, hooks run on changes they've actually reviewed.
- **Multiple agent actions can be grouped into one commit at the human's discretion** ("accept all of linker's suggestions for [[Attention]]"). The audit trail per action lives in `agent_actions`.

**Negative:**

- **`git log --author=<agent>` no longer works.** Audit trail moves to the `agent_actions` SQLite table. Mitigation: an opt-in pre-commit hook can append `engram-actions: <id>, <id>` footers to commit messages, surfacing the reference in `git log`.
- **Long-lived unstaged changes coexist with the indexer.** The file watcher sees and indexes the unstaged content (which is correct --- the working tree is what the user is reading). If the user discards via `git restore`, the next file event reverses the index. This is the right behavior but worth being aware of.
- **Commit-discipline now lives entirely with the human.** A user who never commits accumulates an unbounded unstaged backlog. Mitigation: Pacekeeper monitors backlog depth and throttles agents; the standup nags about uncommitted volume.

## References

- `00-overview.md` --- principle 6, "confidence-gated, git-safe autonomy"
- `01-agents-and-council.md` --- the council state machine's `LAND` outcome
- `03-architecture.md` --- concurrency model (git-write isolation), `agent_actions` schema
- ADR 0004 --- confidence-gated autonomy (the other half of the safety pair)
