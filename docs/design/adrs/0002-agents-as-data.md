# ADR 0002: Agents are directories of prompt + config, hot-reloaded

**Status:** Accepted

**Date:** 2026-04 (during initial design)

## Context

Engram has many agents (the v1 set is 5; the full design has ~35 agents plus a handful of on-demand orchestrators). Each agent has: a system prompt that defines its goals and personality; configuration (schedule, triggers, permissions, model tier, confidence threshold, budget); and access to a subset of the system's tools.

The conventional approach in Rust would be: each agent is a struct implementing an `Agent` trait, registered in code, with prompts as embedded strings or `include_str!()` macros. This makes the agent roster compile-time and the prompts versioned with the binary.

## Decision

**Each agent is a directory under `agents/`** containing:

```
agents/<name>/
├── prompt.md       # the system prompt
├── config.toml     # schedule, triggers, permissions, model, confidence, budget
└── tools.toml      # optional: tool subset override
```

**Prompts and configs are hot-reloaded at runtime.** Editing a prompt file does not require recompiling or restarting engram. Adding an agent means creating a directory --- no code change.

## Alternatives considered

1. **Code-defined agents** with embedded prompts. Conventional, type-safe, but requires recompilation for prompt edits and makes the agent roster a code change.
2. **Database-stored agents.** Configs and prompts in SQLite. Easy to edit programmatically, hard to git-track or review. Rejected.
3. **Hybrid.** Agent registration in code, prompts in files. The middle option. Avoided because it makes adding an agent a two-step process.

## Decision rationale

- **Prompts evolve faster than code.** Tightening a prompt because Watcher noticed drift is a routine operation. Recompiling for every prompt iteration is friction; hot-reload is iteration-friendly.
- **The roster reflects vault content, not engineering decisions.** Cartographer's specialty might shift over time. A user might want a personal "Recipe Curator" agent for their cooking-notes corner. These are not engineering changes; they're vault-curation decisions.
- **Auditor's prompt evolution requires file-level editing.** The prompt-evolution loop produces variant `agents/<name>/variants/<id>.md` files; promotion is a file move. This is much cleaner with file-based agents than code-based.
- **Agent spawning** (a deferred v3 feature) becomes trivial: copying a directory IS the spawn operation.
- **Onboarding clarity.** Reading what an agent does means opening `prompt.md` --- not navigating type definitions.

## Consequences

**Positive:**

- Agent edits are git-tracked at file granularity (one prompt change = one diff).
- The user can fork an agent by copying its directory.
- Rust binary stays focused on infrastructure; agent personality is data.
- Re-roster (enabling/disabling agents) is config-only.

**Negative:**

- **No compile-time validation of prompt-tool interactions.** If a prompt mentions a tool the agent doesn't have, that's a runtime error. Mitigation: a startup validator that loads each agent's config, checks that referenced tools exist, and fails fast on misconfiguration.
- **Schema drift over engram versions.** If `config.toml` adds a new required field in v1.1, existing agent configs need migration. Mitigation: configs include a `schema_version`; the loader applies in-memory upgrades and writes back the new shape on next save.
- **Prompts are not type-checked against the structured-output schema.** A prompt that doesn't ask for a `confidence` field will produce outputs without it. Mitigation: the LLM call wrapper enforces structured output via JSON schema, regardless of what the prompt requests.

## References

- `01-agents-and-council.md` --- agent definition format, config example
- `03-architecture.md` --- `agents/` directory in workspace layout
- ADR 0001 --- Rust everywhere (the file-based approach is also more practical because we're not relying on Rust's type system for prompts)
