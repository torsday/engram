# ADR 0013: Prefer tool-use over LLM generation for deterministic subtasks

**Status:** Accepted

**Date:** 2026-05 (added during the agentic-AI excellence pass)

## Context

LLM calls are the dominant cost driver in agentic systems. They're also the source of most quality variability — non-determinism, hallucination, schema drift. For any given subtask within an agent's work, two implementations exist:

- **Generation:** prompt the LLM and parse its output. Flexible but expensive, slow, and probabilistic.
- **Tool-use:** call a deterministic tool (sqlite query, graph traversal, regex match, embedding lookup, file read). Cheap, fast, and exact.

The temptation when designing agents is to phrase everything as "ask the LLM to figure out X." This produces fragile, expensive agents whose outputs are hard to audit. The alternative — push as much as possible into deterministic tools, and use the LLM only for genuinely fuzzy judgment — produces agents that are cheaper, faster, more inspectable, and more aligned with their stated job.

This pattern is implicit in several agent specs (Linker calls `hybrid_search` to find candidates rather than asking the LLM to invent them; Cartographer reads the index via SQL rather than prompting the LLM to recall it; Gardener pre-verifies dead links via deterministic check before any LLM involvement). But it has never been articulated as a system-wide design principle.

## Decision

**Prefer tool-use over LLM generation for any subtask whose answer is deterministic given the input.** The LLM is reserved for fuzzy judgment, generation of natural language, and synthesis across multiple inputs where no closed-form algorithm exists.

### The principle, made concrete

For every agent's design, walk through the agent's responsibilities and apply this checklist:

1. **Can a deterministic computation produce this output exactly?** → Tool, not LLM.
   - Examples: "list notes in this tag," "find SHA-256 hash of this content," "compute Reciprocal Rank Fusion of two ranked lists," "extract YAML frontmatter from a markdown string," "verify this wikilink targets an existing note."

2. **Can a deterministic computation produce a *candidate set* the LLM then judges?** → Tool produces candidates; LLM judges. Don't ask the LLM to generate candidates.
   - Linker's design: `hybrid_search` produces the top-K candidate notes; LLM judges which (if any) deserve a wikilink. The LLM is **not** asked "what should this note link to?" — it's asked "of these K candidates, which are good links and why?"
   - Cartographer's design: SQL produces the list of new/changed notes; LLM writes the one-sentence summary for each. The LLM is **not** asked "what notes are in the vault?"

3. **Can a deterministic check filter inputs before LLM call?** → Filter first.
   - Gardener pre-verifies dead links in code; the LLM only judges TODO-resolution cases that aren't already obvious.
   - Source Demand pre-classifies sentences as factual claims (regex + heuristic on assertion verbs); the LLM only judges the ones that need citation evaluation.
   - Confidence Annotator pre-extracts confidence-marker absence in code; LLM only suggests markers for unmarked claims.

4. **Can a deterministic post-check validate LLM output?** → Validate, don't trust.
   - Every structured output goes through schema validation (already in design).
   - Every proposed wikilink target is verified to exist before the action lands.
   - Every claimed citation is verified to be findable in the named source (Source Demand's job).

### Architectural support

The agent runner exposes a **tool gateway** with two distinct call types:

```rust
trait ToolGateway {
    /// Deterministic tool call. No LLM involved. Synchronous, fast, free.
    fn call_tool<I, O>(&self, tool: &str, input: I) -> Result<O>
    where I: Serialize, O: DeserializeOwned;

    /// LLM call. Costs tokens. Returns structured output.
    async fn call_llm<O: AgentOutput>(
        &self,
        prompt: PromptStructured,
        tier: ModelTier,
    ) -> Result<O>;
}
```

Every agent's spec (per `12-agent-spec-template.md`) lists:
- Tools the agent calls (deterministic; cheap)
- LLM calls the agent makes (with prompt structure; expensive)

A code review of any agent should be able to count both numbers. Agents with high LLM-call-to-tool-call ratios get scrutinized — usually they have responsibilities that should be pushed down into tools.

### Naming convention

Tools live in `engram-core::tools::` (or appropriate crate) and follow a naming convention:

- `find_*` — deterministic candidate generation (`find_link_candidates`, `find_dead_links`)
- `verify_*` — deterministic validation (`verify_link_target`, `verify_citation_in_source`)
- `compute_*` — deterministic computation (`compute_invasiveness`, `compute_rrf_score`)
- `read_*` / `list_*` — deterministic data access (`read_note`, `list_tags`, `list_neighbors`)

LLM calls don't live in `tools::`; they go through the `call_llm` path on the gateway.

### Cost discipline

The token estimator (per `01-agents-and-council.md` §Cost-aware planning) uses tool-call counts (free) and LLM-call counts (charged) separately. The cost estimate is a function only of LLM calls; tools don't enter the calculation.

### Testability

Tools are deterministic functions with typed inputs and outputs. Unit tests are trivial (per `13-testing-strategy.md`). LLM calls require mock providers and snapshot/property tests. Pushing logic into tools makes the testable portion grow and the harder-to-test portion shrink.

## Alternatives considered

1. **Let agents prompt the LLM for everything.** Maximum flexibility. Rejected: cost is 5-50× higher; quality is variable; output is hard to audit; testability is low.
2. **Pure tool-based agents (no LLM).** Cheapest. Rejected: defeats the purpose of agentic design; many engram tasks (synthesis, critique, judgment) genuinely require LLM capability.
3. **Heuristic per-agent tradeoff (no system principle).** Rejected: without a stated principle, agent design drifts toward LLM-heavy because that's the easier path to express in a prompt; no architectural pressure to push subtasks into tools.
4. **Tool-use over generation as a stated principle, enforced via review and design template.** Chosen.

## Decision rationale

- **Cost.** Each tool call replaces an LLM call worth $0.001-0.025. Across ~35 agents and high-frequency triggers, the aggregate is meaningful.
- **Speed.** Tool calls are sub-millisecond; LLM calls are seconds. Latency-sensitive paths benefit substantially.
- **Quality.** Deterministic tools don't hallucinate. Pushing factual lookups (does this note exist? what tags does it have? what's its content hash?) into tools eliminates a class of agent failure where the LLM confidently states something false.
- **Testability.** Tools are unit-testable; LLM calls require infrastructure. A higher tool-to-LLM ratio means more of the system is verifiable.
- **Inspectability.** A tool call's input and output are clean. An LLM call's "reasoning" is opaque (and not the canonical output anyway). Tool-heavy agents are easier to debug.
- **Aligns with token efficiency** (companion to ADRs 0010 and 0011): the cheapest LLM call is the one that doesn't happen.

## Consequences

**Positive:**

- **Real cost reduction** for any agent that adopts the pattern thoughtfully. Linker's design (tools generate candidates; LLM judges) is the exemplar; other agents apply the same shape.
- **Faster agents** — most observable in the diff queue and council convene-to-quorum latency.
- **More auditable behavior** — when an agent does something wrong, the failure is in either a tool (deterministic, fixable) or an LLM call (probabilistic, tunable via prompt evolution). Each has clear remediation paths.
- **Better unit-test coverage** by construction.
- **Composes with ADRs 0010 (prompt caching) and 0011 (tier escalation):** fewer LLM calls means caching savings are concentrated and tier escalation churn is bounded.

**Negative:**

- **Discipline required at design time.** Agent authors must consciously look for "could this be a tool?" rather than reflexively reaching for the LLM. Mitigation: the agent spec template includes the checklist explicitly; reviews flag LLM-heavy agents.
- **Some judgments resist tool-ification.** Synthesizer's "is this a meaningful concept?" can't be a tool; it's irreducible LLM work. That's fine — the principle is "prefer tool-use *where applicable*," not "eliminate LLM use."
- **Tool growth is also a cost.** Adding 50 specialized tools to support agents means 50 more functions to maintain and test. Mitigation: keep tools small and composable; reuse aggressively (`find_neighbors` is shared across many agents); resist single-use tools.
- **The principle is a design-time check, not an enforced runtime constraint.** Mitigation: agent specs (`12-agent-spec-template.md`) record tool list AND LLM-call structure; deviations are visible in code review and Auditor's qualitative pass.

## References

- [`01-agents-and-council.md`](../01-agents-and-council.md) — agent design philosophy; structured output schemas
- [`12-agent-spec-template.md`](../12-agent-spec-template.md) — every agent spec lists tools separately from LLM-call structure; v1 agent specs (Linker, Gardener, Cartographer, Scribe, Ingestor) all illustrate the pattern
- [`13-testing-strategy.md`](../13-testing-strategy.md) — tools are unit-tested; LLM calls require mock providers
- [ADR 0010](0010-prompt-caching-first-class.md) — companion: when LLM calls are necessary, make them cache-friendly
- [ADR 0011](0011-tiered-model-escalation.md) — companion: when LLM calls happen, start cheap
