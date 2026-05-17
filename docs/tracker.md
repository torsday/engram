# Tracker conventions

Engram's work lives in [GitHub Issues](https://github.com/torsday/engram/issues) and the [Engram project board](https://github.com/users/torsday/projects/11). This doc records the conventions that make those work together — if any of these change, update this file.

## The board

One project board: [**Engram** (#11)](https://github.com/users/torsday/projects/11), user-owned (not org-owned). All open repo issues auto-include via GitHub's repo-wide workflow.

### Fields (all single-select unless noted)

| Field                  | Values                                                                                                     | What it answers                                                                                                                                                                           |
| ---------------------- | ---------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Status**             | `Backlog` → `Up Next` → `In Progress` → `In Review` → `On Hold` → `Done`                                   | Where is this in the lifecycle? Items move left-to-right (with `On Hold` as a sidetrack from any non-terminal column).                                                                    |
| **Priority**           | `P0 · critical` / `P1 · high` / `P2 · medium` / `P3 · low`                                                 | How urgent is this relative to other work?                                                                                                                                                |
| **Size**               | `XS` (hours) / `S` (~1d) / `M` (2–3d) / `L` (4–5d) / `XL` (split it first)                                 | Rough effort estimate. `XL` is a smell — break it down before starting.                                                                                                                   |
| **Risk**               | 🔴 High / 🟡 Medium / 🟢 Low                                                                               | If this goes sideways, how disruptive? Calibrate against "needs careful design + review" (High) vs "follows a pattern" (Low).                                                             |
| **Sprint** (iteration) | Defined per sprint                                                                                         | When do we expect to work this? Optional — leave unset for backlog items.                                                                                                                 |
| **Model Queue**        | `sonnet-low` / `opus-med` / `opus-high` / `opus-1m-max` + `In Progress` / `In Review` / `On Hold` / `Done` | Which Claude tier is right for this work? See [model tiers](#model-tier-labels). The lifecycle options mirror Status so the **ModelBan** view shows in-flight items in lifecycle columns. |

### Views

Two views, both grouped by single-select field:

1. **KanBan** — group by Status. The canonical lifecycle queue. This is what `/ship-next` reads from (it picks from the `Up Next` column).
2. **ModelBan (Agent Board)** — group by Model Queue. Same items, sliced by tier instead of lifecycle. Lets you spin up opus-only or sonnet-only sessions and pick the next available work in that tier.

> [!NOTE]
> View creation isn't supported by `gh` CLI — both views are configured manually in the GitHub UI. If you're rebuilding the board, create them by hand. Other than view-grouping, everything else (fields, options, item membership) lives in `scripts/file-issue.sh`.

## Filing issues

### Always use `scripts/file-issue.sh`

GitHub Projects v2 is multi-step: create the issue, add it to the project, set Status, set Priority, set Size, set Risk, set Model Queue. Manual `gh issue create` invocations frequently leave fields empty, producing items that don't show up in filtered views and disappear from the team's attention.

The script does all of it atomically:

```bash
scripts/file-issue.sh \
  --title "Add JWT refresh token rotation" \
  --body-file path/to/body.md \
  --milestone "v1.0-runtime" \
  --labels "type: feature,P1 · high,size: M" \
  --status "Up Next" \
  --priority "P1 · high" \
  --size "M" \
  --risk "🟢 Low" \
  --model "sonnet-low"
```

After creating + wiring, it re-reads the project item and prints the field values for verification. The mutation-success response from GraphQL is not proof — the round-trip is.

### Body format

Issue body should follow this structure (the GitHub Issue Forms templates at `.github/ISSUE_TEMPLATE/*.yml` enforce this in the UI):

```markdown
## Summary

One paragraph: what work this issue tracks, and why.

## Acceptance criteria

- [ ] Observable, testable outcomes — not implementation steps.
- [ ] Each item is something a reviewer can verify is done.

## Design references

- `docs/design/XX-relevant-doc.md` — §section
- ADR NNNN if applicable

## Dependencies

- Blocked by: #N
```

### Titles

Verb-first imperative. Type goes on the label, not in the title.

| Good                                                   | Bad                           |
| ------------------------------------------------------ | ----------------------------- |
| Add lean frontmatter parser/serializer with serde_yaml | [Feature] Frontmatter parsing |
| Fix race condition in proposal writer                  | Bug: proposals broken         |
| Evaluate LanceDB vs sqlite-vec for embeddings          | Investigate vector storage    |

## Labels

Three tiers, by mutual-exclusion semantics:

| Tier                                                         | Used for                                          | Examples                                                          |
| ------------------------------------------------------------ | ------------------------------------------------- | ----------------------------------------------------------------- |
| **Project field** (single-select, enforced)                  | Scheduling dimensions the board UX needs          | Status, Priority, Size, Risk, Model Queue                         |
| **Namespaced label** `category: value` (discipline-enforced) | Mutually exclusive dimensions used in CLI filters | `type: feature`, `type: bug`, `type: docs`, `model: opus-med`     |
| **Bare label**                                               | Non-exclusive flags                               | `security`, `regression`, `breaking-change`, `flaky`, `tech-debt` |

### Type labels (Conventional-Commits aligned)

`type: feature` · `type: bug` · `type: chore` · `type: refactor` · `type: perf` · `type: docs` · `type: test` · `type: infra` · `type: spike` · `type: epic`

### Model tier labels

Every open issue gets exactly one of `model: sonnet-low` / `model: opus-med` / `model: opus-high` / `model: opus-1m-max`. Decision rule and tier resolution live in [`~/.claude/skills/shared/model-tiers.md`](../../.claude/skills/shared/model-tiers.md) (global, not in this repo).

In short:

- **`sonnet-low`** — translation surfaces, thin wrappers, scaffolding that follows a pattern, deterministic CLI work
- **`opus-med`** — most agent implementations, the API layer, anything requiring judgment but not deep reasoning
- **`opus-high`** — multi-system coordination, deliberation engines, cross-cutting infrastructure
- **`opus-1m-max`** — full-vault sweeps, the real-vault smoke test, anything where 200K context isn't enough

## Milestones

Group by deliverable outcome, not by team or component. Each milestone answers: _"What can the system or user do when this is done?"_

v1 milestones:

| Milestone                | What it ships                                                                                                                             |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| **v1.0-foundation**      | Vault, indices, git boundary, embeddings, hybrid retrieval. The bones.                                                                    |
| **v1.0-runtime**         | LLM providers, prompt caching, agent runner, cost ceiling, council deliberation, eval framework, REST/SSE API, CLI surface. The engine.   |
| **v1.0-agents-core**     | The 5 foundational agents + Inbox Triage. The first set of useful work.                                                                   |
| **v1.0-agents-thinking** | The full thinking + personal + temporal + pedagogical + structural layer (~19 agents). The smart stuff.                                   |
| **v1.0-curator**         | Curator + corpus digestion pipeline. The "import my 9K Obsidian vault" surface.                                                           |
| **v1.0-mcp**             | Internal MCP server (stdio) — engram from inside Claude Desktop/Code.                                                                     |
| **v1.0-onboarding**      | First-run wizard + bootstrap mode + tutorial vault. Day-one safety.                                                                       |
| **v1.0-swift**           | iOS + macOS universal app. Capture, review, browse, conversation, widgets.                                                                |
| **v1.0-acceptance**      | SPEC.md runner, e2e scenarios, perf budgets, threat model, recovery, real-vault test, docs, release engineering. The ship-readiness gate. |

External-facing surfaces (external MCP, Auditor, Pacekeeper) are v2+ per [`docs/design/07-roadmap.md`](design/07-roadmap.md) — don't file v2 work into v1 milestones.

## Epics + sub-issues

Each multi-issue milestone has a single epic issue (`type: epic`) wired as the formal GitHub parent issue of its children. The epic body holds the milestone's task list; the GitHub UI then shows progress against the epic automatically. Don't manually maintain `## Tasks` checkbox state in the epic body — let the sub-issue closures drive it.

## Status transitions

- `Backlog` → `Up Next` — when you've decided this is next-to-work and dependencies are met
- `Up Next` → `In Progress` — when you start (one item at a time per worker)
- `In Progress` → `In Review` — when a PR is opened
- `In Review` → `Done` — when the PR merges (auto-set by project workflow)
- _any non-terminal_ → `On Hold` — when blocked by external state; comment on the issue with what unblocks it
- `On Hold` → wherever it came from — when unblocked

## When not to file an issue

- Vague goals — write a spec first (`/spec`)
- Architecture decisions — write an ADR (`/adr`); only file an issue if the ADR proposes implementation work
- Mid-conversation observations — flag via `spawn_task` instead, which spins off a separate session
- Skill changes — edit `~/.claude/skills/<skill>/SKILL.md` directly; skills aren't tracked here
