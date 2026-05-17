# ADR 0011: Tiered model escalation (start cheap, escalate on need)

**Status:** Accepted

**Date:** 2026-04 (added during the token-efficiency design pass)

## Context

Engram exposes three model tiers (`fast`, `standard`, `deep`) that map to concrete provider models (default: Haiku, Sonnet, Opus on Anthropic; equivalent OpenAI / Ollama tiers on those providers). Each agent's `config.toml` specifies its `model_tier`.

The original design fixes the tier per agent: Linker is always `fast`, Synthesizer is always `standard`, Heretic and Analogist are always `deep`. This is simple and predictable but pessimistic — it assumes every call needs the agent's "ceiling" model. In practice, **most calls succeed at the cheapest tier**: the obvious link, the unambiguous tag, the well-formed structured output. A small fraction of calls genuinely need the more capable model: the ambiguous content, the malformed initial response, the low-confidence first-pass result.

Cost cap is per-month aggregate; per-call cost matters too. Spending Opus tokens on every Heretic call wastes money on the easy 80% of cases that Sonnet would handle just as well.

## Decision

Each agent specifies a **tier escalation policy** in addition to (or instead of) a fixed `model_tier`. The runner attempts the cheapest tier first and escalates only when warranted by the response.

### Escalation policy in `config.toml`

```toml
[agent]
name = "heretic"
escalation_policy = "ladder"   # ladder | fixed
start_tier = "fast"            # the bottom of the ladder
ceiling_tier = "deep"          # the top; no escalation past this
escalate_on = ["confidence_below_0.6", "schema_invalid", "explicit_request"]
max_escalations_per_call = 2   # fast -> standard -> deep is two escalations
```

For agents that should always run at a fixed tier (e.g., Witness, which has no quality-vs-cost tradeoff because it never modifies the vault):

```toml
escalation_policy = "fixed"
model_tier = "fast"
```

### Escalation triggers

The runner escalates when ANY of these conditions hold after a call at the current tier:

1. **`confidence_below_0.6`** — the agent's self-reported confidence is below a threshold (default 0.6, configurable per agent). The lower-tier model probably wasn't sure; try the higher tier.
2. **`schema_invalid`** — the response failed to parse against the agent's structured-output schema. The lower-tier model produced malformed JSON or missed required fields. Almost always indicates the model was over-its-head; escalate.
3. **`explicit_request`** — the agent's structured output may include an `escalate: true` field as a self-aware signal. ("This input is more complex than I can handle reliably; please retry me at a higher tier.") Optional per agent.
4. **`schema_drift`** — over the recent 100 calls, schema-validity rate at the current tier is < 95%. Persistent under-performance triggers a tier-floor adjustment via Watcher (independent of any single call).

### Escalation flow

```
attempt 1 at start_tier
  -> structured output parsed?
       -> No (schema_invalid): escalate to next tier (if available); attempt 2
       -> Yes:
          confidence >= 0.6 AND escalate != true?
            -> Yes: accept response, return
            -> No: escalate to next tier; attempt 2

attempt 2 at next_tier (e.g., standard)
  -> same checks; escalate to ceiling_tier if needed; attempt 3

attempt 3 at ceiling_tier
  -> if still failing after ceiling, return whatever we have with low_confidence flag
     and increment "ceiling_failure" metric
```

Bounded by `max_escalations_per_call` (default 2; fast → standard → deep covers all three tiers).

### Cost accounting

Each tier's tokens are charged separately. A call that escalated `fast → standard` charges for _both_ attempts (the fast attempt was a real call). The cost estimator accounts for this by computing an _expected_ cost based on historical escalation frequency:

```
expected_cost = base_tier_cost × (1 + escalation_rate × escalation_cost_multiplier)
```

`escalation_rate` is the fraction of recent calls that escalated; `escalation_cost_multiplier` reflects that each escalation step is 5-10× more expensive.

### Calibration loop

Watcher tracks per-agent escalation rate. Two failure modes to detect:

1. **Escalation rate too high** (> 30%): the `start_tier` is too low for this agent. Watcher proposes raising `start_tier` (e.g., agent should start at `standard` instead of `fast`). User approves via Trust ceremony.
2. **Escalation rate near zero** (< 1%) AND ceiling-tier-only output quality is no better than start-tier output: the `ceiling_tier` is unnecessarily high. Watcher proposes lowering `ceiling_tier` to save the tail cost.

### Eval-framework integration

The eval framework (per `01-agents-and-council.md` §Eval framework) runs each agent at its `start_tier` against the case suite and records pass rate + cost. Agents with high pass rate at the cheap tier should not have their `start_tier` raised by Watcher; this signal protects against premature escalation.

## Alternatives considered

1. **Fixed tier per agent (status quo before this ADR).** Simple, predictable, but wasteful on the easy majority of calls.
2. **Always start at the cheapest tier; never escalate.** Cheapest. Rejected: schema-invalid and low-confidence outputs land in the vault unfiltered, eroding quality. Cost savings come at quality cost.
3. **LLM-based tier router (a separate small model decides which tier to use upfront).** Adds latency and cost (an extra call per agent invocation). Rejected: the routing decision is no easier than the work itself.
4. **Per-input-complexity heuristic (e.g., "if note > 1000 words, start at standard").** Brittle and hard to maintain. Rejected.
5. **Tiered escalation triggered by confidence + schema validity.** Chosen.

## Decision rationale

- **Most calls land at the cheap tier.** Empirically, well-prompted Haiku handles the majority of Linker/Gardener/Cartographer/Scribe work; only a fraction needs Sonnet, and a smaller fraction needs Opus. Pre-paying Opus rates for every call is wasteful.
- **Quality is preserved.** When the cheap tier produces a low-confidence or malformed response, escalation catches it; the user never sees worse output than the fixed-tier design would have produced.
- **Cost reduction is meaningful.** A typical Linker call: `fast` ≈ $0.0008, `standard` ≈ $0.005, `deep` ≈ $0.025. If 80% land at fast, 15% at standard, 5% at deep: average call cost ≈ $0.0024 vs. $0.025 if always at deep — **10× cost reduction** with quality preserved.
- **Self-correcting via Watcher.** The system learns which tier is right for each agent over time; the user doesn't have to guess.
- **Composable with prompt caching (ADR 0010).** Escalation calls inherit the cache structure; a `fast → standard` escalation re-uses the cached static head if within the 5-minute window.

## Consequences

**Positive:**

- **Substantial per-call cost reduction** for agents that run frequently.
- **Quality preserved** by automatic escalation on confidence/validity failure.
- **No upfront tier-tuning required.** Agents start with a sensible default (`start_tier = "fast"` for most) and Watcher tunes via observation.
- **Correlation IDs and `agent_actions` rows** record which tier(s) each call used, enabling cost attribution and tuning.

**Negative:**

- **Latency penalty on escalation.** A `fast → standard → deep` chain takes 3× as long as a single Opus call. Mitigation: most calls don't escalate; for latency-sensitive paths (Pair-Thinking conversations), agents may be configured `escalation_policy = "fixed"` at `standard`.
- **Cost double-charging on escalation.** A call that escalates pays for both tiers. Mitigation: net cost is still much lower than always-at-ceiling for typical escalation rates; cost estimator accounts for it.
- **Schema validity is sometimes legitimately hard to achieve at low tier.** Some agents (Heretic, Analogist) may have legitimately high escalation rates. That's fine — Watcher's calibration loop will raise their `start_tier` once the pattern stabilizes.
- **Witness explicitly opts out of escalation.** Some agents have no quality-vs-cost tradeoff to optimize. Their config sets `escalation_policy = "fixed"`.

## References

- [`01-agents-and-council.md`](../01-agents-and-council.md) — model tier mapping; agent config; eval framework; cost-aware planning
- [`03-architecture.md`](../03-architecture.md) — `engram-llm` provider abstraction, model tier mapping config
- [ADR 0010](0010-prompt-caching-first-class.md) — prompt caching (composes with escalation)
- [`12-agent-spec-template.md`](../12-agent-spec-template.md) — agent spec template now includes `escalation_policy`
