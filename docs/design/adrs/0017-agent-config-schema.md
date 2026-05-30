# ADR 0017: One canonical `AgentConfig` schema, owned by `engram-core`

**Status:** Accepted

**Date:** 2026-05 (documents the reconciliation that unblocked the per-agent issues — Steelman #51, Linker #41, and the wider agent roster)

## Context

Two Rust types deserialized the same file — `agents/<name>/config.toml`:

1. **`engram_core::config::AgentConfig`** (`crates/engram-core/src/config.rs`). Spec-derived, mirroring the Linker example in `docs/design/01-agents-and-council.md` §Linker: a nested `[agent]` table (`name`, `description`, `model_tier`) plus `[schedule]`, `[permissions]`, `[autonomy]`, `[council]`, `[memory]`, `[trust]`, `[budget]`, `[conversation]` subsections. Every struct carries `#[serde(deny_unknown_fields)]` so typos fail loudly. Loaded by `engram-cli` to determine the model tier (which flows into [ADR 0011](0011-tiered-model-escalation.md)'s tiered escalation).

2. **`engram_agents::runner::AgentConfig`** (`crates/engram-agents/src/runner.rs`). A flat, runner-shaped view: top-level `name`, `trigger`, `confidence_threshold`, `max_invasiveness`, `cron_interval_secs`. No `deny_unknown_fields`. Loaded by `AgentRunner::load_cached` on every run.

The two schemas were **mutually exclusive**. engram-core's parser rejects a top-level `name = "..."` (unknown field outside `[agent]`); engram-agents's parser required it there. While the repo had no agent files this was latent, but the first agent slice (#51 Steelman) hit the wall immediately: a `config.toml` written for one parser failed the other.

A single file with two incompatible readers is a defect, not a design. The fix had to (a) keep `engram-cli`'s rich model-tier/permissions/budget structure, which is spec-canonical and already wired into cost caps and council routing, and (b) keep the runner's hot path cheap — it only reads four or five fields and shouldn't be coupled to every future config subsection.

### Options considered

- **Option A — unify on engram-core's schema; runner projects a minimal view.** engram-agents reads the nested shape and projects the fields it needs. Smallest engram-core change; the richer, spec-aligned schema wins. _(chosen)_
- **Option B — unify on engram-agents's flat schema.** Strip engram-core down to the flat shape. Discards the designed permissions/council/budget/trust structure already consumed by `engram-cli` and the budget system (#308). Rejected: throws away working, spec-mandated structure.
- **Option C — move `AgentConfig` to a third shared crate.** Architecturally cleanest in the abstract, but engram-core _is_ the shared foundation crate — engram-agents already depends on it. A new crate adds a hop for no gain.
- **Option D — make engram-core tolerate the flat shape** (drop `deny_unknown_fields` or add `flatten` aliases). Pragmatic stopgap, but leaves two latent schemas and forfeits the typo-detection that `deny_unknown_fields` buys. Rejected as the long-term answer.

## Decision

**`engram-core` owns the one canonical `AgentConfig` schema** — the nested, spec-derived shape with `deny_unknown_fields` on every struct (Option A). engram-agents does **not** define a competing wire format; its `runner::AgentConfig` is a _projection_, not a parser of record.

Concretely:

1. **Canonical parse path.** `engram_agents::runner::AgentConfig::from_toml` attempts `toml::from_str::<engram_core::config::AgentConfig>` first, then projects via `from_core` into the runner's flat view. Production agent files (`agents/<name>/config.toml`) all use the nested shape and flow through this path; it is the same parse `engram-cli` uses for model-tier lookup, so the two crates can never disagree about what a given file means.

2. **Projection drops what the runner doesn't read.** `from_core` maps `[agent].name`, `[schedule].trigger`, `[autonomy].auto_land_min_confidence` (f64 → f32), and `[permissions].max_invasiveness` into the runner view. Everything else (`permissions.may_*`, `council.*`, `memory.*`, `trust.*`, `budget.*`, `conversation.*`) is dropped on the floor — adding a field to the runner is an incremental, additive change, not a schema migration. `cron_interval_secs` has no analogue in engram-core's `cron: String` (a cron _expression_), so it defaults to 60s; a future slice can surface it explicitly if needed.

3. **A legacy flat fallback remains, scoped to tests.** When the nested parse fails, `from_toml` falls back to the old flat shape. This exists **only** so the unification didn't cascade into rewriting ~140 inline TOML strings in the runner's own unit tests. It is transitional: file an issue to migrate those fixtures and drop the fallback. No production file uses it.

4. **One canonical fixture, asserted by both crates.** `tests/fixtures/agents/example/config.toml` exercises every supported field at a deliberately non-default value. `engram-core`'s `canonical_example_fixture_parses_every_field` checks every field round-trips through its schema; `engram-agents`'s `from_toml_accepts_canonical_example_fixture` checks the same file parses and projects correctly. The shared file is the contract; drift between the two deserializers fails CI.

5. **On-disk smoke test.** `engram_agents`'s `on_disk_agent_files_parse` auto-discovers every `agents/<name>/` and asserts it parses via `from_toml` with `[agent].name` matching the directory name — so a new agent's config cannot silently regress the canonical schema.

`model_tier` lives in `[agent]` and is read only by `engram-cli` (via engram-core directly, not the runner projection); this keeps [ADR 0011](0011-tiered-model-escalation.md)'s tier selection sourced from the single canonical schema.

## Consequences

**Positive.**

- One source of truth for `config.toml`. The richer spec-aligned schema (permissions, council, budget, trust, memory, conversation) is preserved and remains the format every agent file is written in.
- `deny_unknown_fields` keeps catching typos across the whole config surface.
- The runner stays decoupled from config growth: new subsections don't touch the hot path until the runner actually needs a field.
- The per-agent issues (#51, #41, #44, #43, #49, #50, #56, #57, #58, …) are unblocked — every agent's first slice writes one nested `config.toml` that both crates accept.

**Negative / costs.**

- Two type _shapes_ still exist in the codebase (canonical + projected view), even though only one _wire format_ does. The projection must be kept in sync by hand when the runner needs a new field — mitigated by the shared fixture test failing loudly on drift.
- The legacy flat fallback is dead weight until the runner's test fixtures migrate. It is deliberately retained as transitional debt with a tracking issue, not silently kept forever.

## References

- `docs/design/01-agents-and-council.md` §Linker — the spec-canonical `config.toml` shape.
- [ADR 0011](0011-tiered-model-escalation.md) — tiered model escalation; `model_tier` is sourced from this schema.
- [ADR 0002](0002-agents-as-data.md) — agents are directories of `prompt.md` + `config.toml`.
- [ADR 0016](0016-per-agent-typed-outputs.md) — the parallel decision for agent _output_ schemas (engram-agents owns those; engram-core owns _config_).
- `crates/engram-core/src/config.rs` — canonical `AgentConfig`.
- `crates/engram-agents/src/runner.rs` — `from_toml` / `from_core` projection.
- `tests/fixtures/agents/example/config.toml` — the shared canonical fixture.
