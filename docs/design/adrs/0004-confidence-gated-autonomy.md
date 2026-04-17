# ADR 0004: Per-action confidence threshold gates autonomous writes

**Status:** Accepted

**Date:** 2026-04 (added when the original "trust score gates auto-land" model was refined)

## Context

The original autonomy model was binary per agent: trust scores classified an agent as `low`, `medium`, or `high`, and that classification determined whether changes auto-landed or went through council. This was too coarse. A high-trust Linker would auto-land confident proposals AND uncertain proposals --- losing the agent's own judgment about which proposals it actually believed in.

The natural refinement: have each agent self-assess confidence per action. The system then routes based on confidence.

## Decision

**Every agent action carries a self-assessed `confidence` score (0.0--1.0).** Each agent's `config.toml` specifies an `auto_land_min_confidence` threshold (default 0.85, override `0.95` in bootstrap mode).

- `confidence ≥ threshold` AND change is at-or-below the agent's `max_invasiveness`: the agent writes directly to the working tree (still unstaged per ADR 0003).
- `confidence < threshold` OR invasiveness exceeds the ceiling: the action becomes an explicit proposal through council.

Confidence is computed from:
1. **LLM self-score** in the agent's structured output (a `confidence` field).
2. **Retrieval-signal agreement** (BM25 + dense + graph all converging raises confidence; conflict lowers it).
3. **Calibration adjustment** (Watcher tracks claimed-vs-actual acceptance per agent over time; agents that overstate get auto-corrected via prompt evolution).

Trust scores survive but **modulate the threshold** rather than gate the action: high-trust agents may be allowed to use a relaxed threshold (e.g., 0.80 instead of 0.85); low-trust agents are forced to a stricter one (e.g., 0.95). The Watcher manages this per the agent's `trust_modulates_threshold` config.

## Alternatives considered

1. **Trust scores alone (original).** Rejected: too coarse; ignores per-action variation in agent confidence.
2. **Confidence alone.** No trust scores. Rejected: a chronically miscalibrated agent gets no system-level corrective. Trust modulation closes the loop.
3. **Council for everything.** Conservative but expensive. Rejected: defeats the value of mechanical agents being able to act quietly.
4. **Confidence + invasiveness ceiling + trust modulation.** Chosen.

## Consequences

**Positive:**

- **The agent's own judgment matters.** An action it believes in lands; an action it's uncertain about gets reviewed.
- **Calibration becomes a first-class metric.** The system rewards calibration, not optimism. Agents that overstate get tuned down.
- **Pairs with ADR 0003 to form a two-layer safety net.** Confidence gates what reaches the working tree; git gates what reaches history.
- **Easy to dial.** A user worried about a particular agent raises its threshold to 0.95 and gets propose-only behavior without disabling.

**Negative:**

- **Self-reported confidence is a noisy signal.** LLMs are bad at calibration out of the box. Mitigation: the calibration loop (Watcher → prompt evolution) closes over time. Bootstrap mode uses a high (0.95) override that's robust to early miscalibration.
- **Confidence-aware prompts add complexity.** Every agent prompt must include the "rate your confidence honestly" instruction and the structured-output schema must include the field. Mitigation: a shared prompt template for all agents handles this consistently.
- **Adversarial input could manipulate confidence.** A malicious note (via corpus digestion or external source) could craft content that triggers high LLM confidence on a wrong action. Mitigation: see threat model (`09-threat-model.md`); structured outputs and council oversight bound the blast radius.

## References

- `00-overview.md` --- principle 6, "confidence-gated, git-safe autonomy"
- `01-agents-and-council.md` --- "Confidence-gated autonomy" section
- ADR 0003 --- no agent commits (the git half of the safety pair)
