# First Run and Onboarding

## Purpose

Every system-level feature in the agent design (`01-agents-and-council.md`) describes the **steady state**: trust scores have history, agent memory has accumulated, prompt evolution has data, the Biographer has read months of writing, the Voice Keeper has a robust voice model, calibration has resolved predictions to learn from. None of this is true on day one.

This document covers the **bootstrap problem**: how engram behaves usefully and safely when it knows almost nothing about the user, and how the system gracefully transitions to the steady-state design as data accumulates.

## The bootstrap problem

The mature design assumes:

- Trust scores per agent are calibrated against acceptance rate.
- Voice Keeper has analyzed enough human-written content to detect drift.
- Biographer has a coherent user model.
- Predictor has resolved enough predictions to compute calibration.
- Pacekeeper has measured the user's review cadence.
- Auditor has quarterly samples to evaluate against.
- Cost spend has stabilized into predictable patterns.

On day one, **none of these are true.** Every dependent agent is starting from zero. Naive defaults would be wrong (agents acting confidently when there's no calibration evidence; Biographer fabricating identity claims from a 12-note vault).

The bootstrap principle: **err toward humility, propose generously, decide nothing autonomously until evidence accumulates.**

---

## First-run wizard

On first launch (Swift app or `engram init` CLI), the wizard runs through:

### 1. Vault selection

- **Greenfield:** create a new vault directory, initialize git, populate with a tutorial `welcome.md` and a few sample notes that demonstrate conventions.
- **Existing Obsidian vault:** point engram at an existing vault. Engram adds `.engram/` and `id:` frontmatter to existing notes (proposed unstaged for the user to review and stage).
- **Migration from prior corpus:** point engram at one vault to use, plus a separate corpus to digest later via Curator.

### 2. Model provider setup

- Local (default): Ollama or in-process ONNX --- no cloud, no keys needed.
- Hybrid: local embeddings, cloud generation --- prompts for Anthropic API key (stored via Keychain).
- Cloud: cloud embeddings + cloud generation --- prompts for keys.

User can change at any time via `engram secrets`.

### 3. Cost cap

Default `monthly_usd_cap = 25.0` (conservative). User can raise. Suggested ranges based on vault size and selected providers shown for guidance.

### 4. Backup configuration

The Backup Watcher needs to know what to monitor:

- Git remote URL (or "I'll set this up later")
- Time Machine status (auto-detected on macOS)
- Optional: artifact remote (S3 / Backblaze)

Warns clearly if no backup is configured. Does not block --- the user can defer, but standup will nag.

### 5. Privacy zones

Defaults proposed:

- `notes/work/` --- local-only processing
- `notes/medical/` --- local-only
- `notes/journal/` --- local-only, eligible for Witness

User accepts/edits.

### 6. Initial agent selection

Defaults: the full v1 agent roster all enabled. User can disable any. v2+ agents (Auditor, Pacekeeper, Untangler, Research Council, Analogist, Scout, Fact Checker, etc.) appear here only when their version of engram is installed.

### 7. Tutorial offer

"Spend 5 minutes learning engram's conventions?" --- opens the welcome note, shows the diff-review interface with a synthetic example, demonstrates capture from the Swift app.

---

## First-30-days mode

When a vault has been active for less than 30 days **or** has fewer than 100 notes, engram enters **bootstrap mode**. This mode adjusts agent behavior in three ways:

### 1. Conservative confidence thresholds

Every agent's `auto_land_min_confidence` is overridden to **0.95** in bootstrap mode (typical default is 0.85). The result: almost everything goes through the diff-review queue rather than landing autonomously. The user sees more of what agents would do, builds an intuition for which agents to trust, and gives Watcher data to calibrate against.

After 30 days _and_ at least 50 resolved diff-reviews, the override is lifted; agents resume their configured thresholds.

### 2. Heightened transparency

Bootstrap-mode notifications are slightly more verbose:

- The standup includes a "what the swarm tried" summary section, not just "what's pending."
- Each diff in the review queue includes a longer rationale by default.
- Agent run failures surface to the user (not just to Watcher), so the user develops a feel for which agents are working.

### 3. No deep evaluation yet

Auditor's quarterly evaluation is skipped until enough decisions exist (default: 50 per agent). Trust scores stay at "medium" --- no promotions, no demotions. Pacekeeper's throttle uses absolute backlog rather than rate-of-change (no rate-of-change history yet).

### Transition out

When all of these hold:

- Vault age ≥ 30 days
- Note count ≥ 100
- ≥ 50 resolved decisions across the swarm

Engram displays a one-time notification: "Engram has graduated from bootstrap mode. Auto-land thresholds are now per-agent; trust scores are active; Auditor will run its first quarterly evaluation in [N days]." The user can extend bootstrap mode manually if they prefer.

---

## Sparse-content bootstrap of context agents

Several agents depend on having content to read. When that content doesn't exist yet, they need to fail gracefully rather than fabricate.

### Biographer

Until the vault has ≥ 200 human-authored notes spanning ≥ 60 days:

- Biographer does not write `meta/biography.md`.
- Other agents that read biography fall back to a stub: "Biographer has insufficient data; user model unavailable."
- The Swift app `/biography` endpoint returns an explicit "not yet available" with the conditions needed to populate.

When conditions are met, Biographer writes its first model. The model itself is conservative on day one --- it should read more like "the user has been writing about X, Y, Z; recurring themes appear to include A, B" than like an authoritative identity statement.

### Voice Keeper

Voice analysis requires a baseline:

- < 50 human-authored notes: Voice Keeper is **observe-only**. It builds the voice model passively but does not participate in council or critique agent output.
- ≥ 50 notes: Voice Keeper joins council, runs in propose-only mode (its rewrites are always reviewed, never auto-landed).
- ≥ 200 notes and ≥ 30 days: Voice Keeper operates per its mature design.

### Predictor

Predictor begins extracting predictions immediately. Calibration profile is not computed until ≥ 10 resolved predictions per topic area (separately per topic). Until then, calibration is reported as "insufficient data." This is honest --- a calibration plot from 3 data points is meaningless.

### Annual Review

Trivially gated: doesn't run until the vault is at least 12 months old. The first Annual Review covers months 1--12; the second covers month 13--24, and so on.

### Tutor

Generates flashcards from any evergreen note. No bootstrap gate --- but the Swift app review interface explicitly notes "your card library is small; sessions will be short" until ≥ 30 cards exist.

---

## Default agent set on day one

Even within the v1 agent set (5 agents), behavior on day one is calibrated:

| Agent        | Day 1 behavior                                                         |
| ------------ | ---------------------------------------------------------------------- |
| Linker       | Active. Threshold 0.95. Most proposals enter review queue.             |
| Gardener     | Active but quiet --- nothing is "stale" yet. Main work: dead-link fix. |
| Cartographer | Active. Generates `index.md` from initial notes.                       |
| Scribe       | Active for fleeting notes only.                                        |
| Ingestor     | Active immediately (file drop and capture both flow through).          |

---

## The seeded vault

For greenfield vaults, the wizard creates a small set of welcome notes that:

- Demonstrate the note conventions (`06-note-conventions.md`)
- Provide examples of each note type (one fleeting, one literature, one evergreen)
- Include a `welcome.md` MOC linking to the others
- Include one note with a known-good wikilink so the new user sees Linker propose connections
- Include one intentional "stale TODO" so the user sees Gardener's first pass

These notes are tagged `engram/tutorial` so Gardener doesn't propose pruning them. The user can delete them at any time.

---

## Onboarding-mode UX in the Swift app

For the first 30 days, the Swift app surfaces a small "Onboarding" panel:

- Day count and note count progress toward graduation
- Three quick-actions: "show me what an agent did," "review my pending diffs," "open the conventions doc"
- Optional walkthrough that triggers each agent at least once with synthetic inputs so the user sees them in action

Dismissable; doesn't reappear once dismissed.

---

## Re-onboarding

When the user pulls a major engram upgrade (e.g., v1 → v2, with new agents enabled), the wizard re-runs in light mode:

- New agents introduced; user accepts/disables each.
- New configuration options surfaced.
- New flows / ceremonies introduced (e.g., Auditor's quarterly review in v2.1) explained.

Bootstrap mode is **not** re-triggered on upgrades --- existing trust scores and history persist.

---

## Anti-patterns to avoid

- **Asking the user every detail upfront.** The wizard is brief by design. Defaults are good. The user can refine later.
- **Fabricating context to fill the void.** Biographer not writing is better than Biographer guessing. A "Voice Keeper has insufficient data" message is better than Voice Keeper enforcing fictional voice rules.
- **Auto-landing aggressively to demonstrate value.** Day-one auto-lands that turn out wrong are the fastest way to lose user trust. Bootstrap mode's high threshold is intentional.
- **Hiding cost from the user.** Cost dashboard is visible from day one; the system warns generously before approaching the cap.
