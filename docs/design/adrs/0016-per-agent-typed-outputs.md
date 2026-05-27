# ADR 0016: Per-agent typed outputs in `engram_agents::agents`

**Status:** Accepted

**Date:** 2026-05 (documents the decision that emerged across PRs #280–#291 implementing the 9 v1 agents)

## Context

Each agent in engram emits JSON-structured output that the runner reads. Three properties matter for that output:

1. **Schema discipline.** Per [ADR 0010](0010-prompt-caching-first-class.md), the static head of an agent's prompt declares the JSON schema; per [ADR 0011](0011-tiered-model-escalation.md), `confidence` and `rationale` must stream first so the runner can early-exit on low-confidence outputs before the expensive payload generates.
2. **Drift detection.** Three artifacts must stay in lockstep: the prompt's documented JSON schema (`agents/<name>/prompt.md`), the Rust-side parse logic, and any test fixtures. A change to any one is a change to all three; silent drift between them is a real bug class.
3. **Hot-path robustness.** The runner's gate logic (read `confidence`, route by invasiveness) must not fail when an agent emits a slightly-malformed payload — a missing optional field shouldn't crash the council.

When the v1 agents were first implemented, the prompts and configs landed on disk (`agents/<name>/{config.toml, prompt.md}`) but the Rust side parsed responses via a permissive `serde_json::Value::get("confidence")` lookup. That's fine for the hot path, but it provides no compile-time guarantee that any specific agent's contract is honored, and it makes prompt-schema changes hard to verify mechanically.

This ADR records the typed-output layer that landed on top of the permissive parse path across the September 2026 implementation sweep.

## Decision

Every agent gets a dedicated submodule under `crates/engram-agents/src/agents/<name>.rs` that mirrors the agent's documented JSON output schema as a Rust type. Specifically:

1. **One module per agent, named after the agent.** `steelman_constructive.rs` for `steelman-constructive`, etc. The on-disk directory name and the Rust module name move in lockstep (`kebab-case` → `snake_case`).
2. **One top-level output struct per agent**, named `<AgentName>Output` (e.g. `SteelmanConstructiveOutput`). Pair-Thinking is the one exception — its conversation-mode contract is one _turn_, not one _output_, so it's named `PairThinkingTurn`.
3. **Field order is the ADR 0011 streaming-early-exit contract**: `confidence` first, `rationale` second, then any flags (`decline`, `mode`, `round`), then the payload. Serde's default behavior emits struct fields in declaration order; the order is pinned in tests so a refactor cannot silently reorder fields.
4. **`#[serde(deny_unknown_fields)]` on every struct** in the agents module. Silent acceptance of unknown fields is the failure mode this ADR exists to prevent: it hides schema drift between the prompt's documentation and the Rust type. The runner's permissive `Value::get` path remains for the hot path; the typed layer is the strict layer.
5. **Mode dispatch is a typed enum**, never a free-form string. Agents with multi-mode dispatch (Inquirer's 4 modes; Voice Keeper's 2; Pair-Thinking's 4 question modes + `End`) carry a `<AgentName>Mode` enum with `#[serde(rename_all = "kebab-case")]` (or `snake_case` for some). A hallucinated mode name fails to parse rather than silently dispatching to a missing handler.
6. **Sub-structs for nested objects.** Agents whose outputs include nested objects (Synthesizer's `ProposedEvergreen`, Splitter's `ProposedSplit`, Bridge Builder's `ProposedBridge`) define those as separate Rust types in the same module — each with `deny_unknown_fields`.
7. **Single dispatch surface via `validate(agent_name, text) -> Result<(), ValidationError>`** in `agents/validate.rs`. Maps agent name to typed parser via an exhaustive `match`. The match-not-HashMap choice is deliberate: a missing arm at compile time means a new agent slipped the typed-struct contract, which is exactly what we want the compiler to catch.
8. **Fixtures lock the three-way contract**: `tests/fixtures/agents/<name>/output/*.json` plus the integration test `crates/engram-agents/tests/fixture_outputs.rs` that walks them through `validate()`. The walker requires each agent to have at least one happy-path fixture and at least one alternate-shape fixture (decline / alternate-mode / end).

The four layers — files on disk, typed structs, dispatch, fixtures — are machine-checked against one another at CI time. The drift modes the integration tests catch:

- New agent has `agents/<name>/` but no `validate()` arm → `ValidationError::UnknownAgent` from the fixture walker.
- Typed struct gains a required field but a fixture doesn't → `ValidationError::ParseFailed` from the fixture walker.
- New agent has files + struct but no fixture → "fixture directory missing" floor assertion fails.
- Refactor reorders struct fields → `serializes_confidence_first` invariant test fails per agent.

## Alternatives considered

**A. Single mega-struct with all fields optional.** Rejected: no compile-time guarantee that any specific agent's required fields are honored. The whole point of typed outputs is that "Steelman's rationale is required" should be checked at parse time, not at runtime via `unwrap()`.

**B. Stay on `serde_json::Value` everywhere.** Rejected: that's the current hot-path behavior and it works fine for the runner's gate, but it provides no surface for eval cases, CLI dry-runs, or schema-drift CI checks to opt into strict validation. The hot-path stays permissive; the typed layer is what consumers reach for when they _want_ strict.

**C. Trait + runtime registration (`Box<dyn AgentOutput>`).** Rejected for two reasons. First, the exhaustive `match` arm in `validate()` lets the compiler catch a missing registration — a runtime registry can have a missing entry and only fail at runtime. Second, the agents have very different output shapes; a `dyn` boundary would either erase the type (losing the compile-time benefits we wanted) or require downcasting (losing the safety we got).

**D. One tagged enum across all 9 agents (`AgentOutput::Steelman { … }` etc.).** Rejected: agents' output shapes differ enough that one enum would be ~300 lines with many irrelevant variants present in every match. Per-agent submodules keep each agent's contract local; callers who care about Steelman only depend on Steelman.

**E. Validate output in the runner's hot path.** Rejected for now (separate ADR if revisited). The runner's `parse_confidence` is intentionally permissive: schema-mismatch should log a warning, not fail the gate. The typed layer's strictness lives behind `validate()` so callers opt in. A future ADR may revisit this once the eval-framework integration lands.

## Consequences

- **The agents/ module is a stable public surface of `engram-agents`.** Future agents follow the same pattern: per-agent submodule, `<Name>Output` struct, `confidence`-first ordering, `deny_unknown_fields`, `validate()` arm, two fixtures.
- **Schema changes are three-file commits.** Updating an agent's JSON schema is a coordinated change: `agents/<name>/prompt.md`, `crates/engram-agents/src/agents/<name>.rs`, and `tests/fixtures/agents/<name>/output/*.json` move together. The integration tests catch any two that drift.
- **The runner's hot path is unchanged.** `parse_confidence` continues to use `serde_json::Value::get`; the typed layer is an opt-in strict surface. No regression risk for the gate.
- **Consumers can choose strict or permissive.** Eval cases, CLI dry-runs, and future runner integration call `validate()`. The runtime hot path keeps the permissive lookup so schema-mismatch never takes down the gate.
- **The `confidence`-first contract is now machine-enforced.** Every agent's typed struct has a `serializes_confidence_first` test that pins the JSON field order. A future refactor reordering struct fields fails this test before it can break the streaming early-exit protocol in [ADR 0011](0011-tiered-model-escalation.md).

The principal cost is duplication: each new agent adds a per-agent module + fixtures rather than fitting into a generic shape. This duplication is the price for the compile-time drift detection across the four artifact types.

## References

- [ADR 0010 — Prompt caching as a first-class design constraint](0010-prompt-caching-first-class.md): static head / dynamic tail split that the output schema lives in.
- [ADR 0011 — Tiered model escalation](0011-tiered-model-escalation.md): the streaming-early-exit protocol that the field-order contract supports.
- [docs/design/12-agent-spec-template.md](../12-agent-spec-template.md) — step 3 ("structured output schema as a Rust type") and step 5 ("test fixtures").
- Implementation PRs: #271–#279 (files on disk, slice 1), #280–#288 (typed structs, slice 2), #289 (`validate()` dispatch), #290–#291 (happy-path + alternate-shape fixtures).
