# ADR 0010: Prompt caching as a first-class design constraint

**Status:** Accepted

**Date:** 2026-04 (added during the token-efficiency design pass)

## Context

Engram makes a lot of LLM calls. With ~35 agents running on a 10K-note vault — many on file-change triggers — the input-token volume is substantial. The same prompt structure is reused across many calls (Linker's prompt for one file-change event looks 95% identical to its prompt for the next file-change event; only the specific note context differs).

Anthropic's API supports **prompt caching** (`cache_control` markers): the static prefix of a prompt is cached on the provider side for ~5 minutes; subsequent requests with the same prefix pay roughly **10% of the input-token cost** for the cached portion. OpenAI has similar mechanisms (automatic prefix caching for prompts > 1024 tokens).

If engram's prompts are designed *without* caching in mind — interleaving static instructions with dynamic note context arbitrarily — the cache hit rate is near zero and the cost stays at full price. If engram's prompts are designed *for* cache friendliness from day one, the typical input cost drops by a large factor.

This is an architectural decision because it affects every agent's prompt structure, the prompt loader's responsibilities, the LLM provider abstraction, and the metrics we track.

## Decision

**Treat prompt caching as a first-class design constraint.** Every agent's prompt is structured as **STATIC HEAD + DYNAMIC TAIL**, and the LLM provider abstraction emits cache-control markers at the boundary.

### Prompt structure

```
[STATIC HEAD — cached]
1. Agent identity and role (rarely changes)
2. Task description and constraints (rarely changes)
3. Output schema directive (rarely changes)
4. Static reference material:
   - Evergreen rubric definition
   - Tag namespace conventions
   - Confidence-rating instructions
   - Biographer model excerpt for the user's general profile (changes monthly)
[/cache marker here]

[DYNAMIC TAIL — not cached, varies per call]
5. Note being analyzed (or other input specific to this call)
6. Retrieval results (semantically similar neighbors, etc.)
7. Recent-changes context
```

The static head is identical across calls within a 5-minute window for the same agent. The dynamic tail is what changes. The cache hit rate for the static head is high; the cost of the dynamic tail is unavoidable.

### Provider abstraction support

The `engram-llm` crate's `LlmProvider` trait emits cache-control markers automatically when the underlying provider supports them:

```rust
pub struct PromptStructured {
    /// Static prefix; provider receives a cache_control marker at the end of this segment.
    pub static_head: String,

    /// Dynamic suffix; never cached.
    pub dynamic_tail: String,
}

impl AnthropicProvider {
    fn build_request(&self, prompt: PromptStructured) -> RequestBody {
        json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt.static_head, "cache_control": {"type": "ephemeral"} },
                    { "type": "text", "text": prompt.dynamic_tail }
                ]
            }],
            ...
        })
    }
}
```

The `OpenAIProvider` lets the static-head structure produce automatic prefix-cache hits without explicit markers (OpenAI caches automatically for prompts > 1024 tokens with consistent prefixes).

The `OllamaProvider` ignores the structure (no caching support), but the prompt still works — split is structural, not semantic.

### Prompt loader responsibilities

The prompt loader (`engram-agents::prompt_loader`) returns prompts as `PromptStructured` rather than `String`. Each agent's `prompt.md` file uses an explicit `<!-- /cache -->` marker to denote the head/tail boundary:

```markdown
You are Linker, an agent in the engram knowledge system. Your job: ...

# Constraints
- ...

# Output
Return ONLY a JSON object matching the LinkerOutput schema.

<!-- /cache -->

# Context
- User biography (if available): {{biography_excerpt}}
- Note being analyzed: {{note}}
- Top neighbors: {{neighbors}}
- Existing wikilinks: {{existing_links}}
```

Everything before `<!-- /cache -->` is the static head. Templating substitution happens only in the dynamic tail.

### Metrics tracked

Per-call:
- `input_tokens_total`
- `input_tokens_cached` (read from provider response)
- `cache_hit_ratio = cached / total`

Per-agent (rolling 30 days):
- Mean cache hit ratio
- Effective cost-per-call vs. uncached cost-per-call

Surfaced in standup and `engram status`. An agent with cache hit ratio < 50% triggers a Watcher finding (likely indicates prompt restructuring needed or biographer-model excerpt being included incorrectly in the dynamic tail).

### Scope of cacheability

| Content                         | Cache placement                                |
| ------------------------------- | ---------------------------------------------- |
| Agent identity / role           | Static head                                    |
| Task instructions / constraints | Static head                                    |
| Output schema                   | Static head                                    |
| Evergreen rubric                | Static head                                    |
| Tag namespace conventions       | Static head                                    |
| Biographer model excerpt        | Static head (changes monthly; cache rebuilds)  |
| Voice model excerpt             | Static head (changes monthly)                  |
| Note being analyzed             | Dynamic tail                                   |
| Retrieval results               | Dynamic tail                                   |
| Recent-changes context          | Dynamic tail                                   |
| Conversation history            | Dynamic tail                                   |

### Cost-estimator integration

The token estimator (per ADR cost-aware planning section in `01-agents-and-council.md`) accounts for cache hits:

```rust
fn estimated_cost_usd(prompt: &PromptStructured, model: &Model, recent_cache_hit_rate: f32) -> f64 {
    let head_tokens = approximate_tokens(&prompt.static_head);
    let tail_tokens = approximate_tokens(&prompt.dynamic_tail);
    let head_cost = head_tokens as f64
        * (recent_cache_hit_rate * model.cached_input_price + (1.0 - recent_cache_hit_rate) * model.input_price);
    let tail_cost = tail_tokens as f64 * model.input_price;
    let output_cost = approximate_tokens(&prompt.expected_output_size) as f64 * model.output_price;
    head_cost + tail_cost + output_cost
}
```

Estimates updated as cache-hit rate calibrates over time.

## Alternatives considered

1. **Don't cache.** Simplest. Rejected: orders of magnitude more expensive than necessary; doesn't scale to ~35 agents on file-change triggers.
2. **Cache opportunistically without prompt restructuring.** Rely on provider's automatic prefix caching where available. Rejected: works for OpenAI but not Anthropic (which requires explicit `cache_control` markers); inconsistent behavior across providers makes cost unpredictable.
3. **Cache the whole prompt.** Rejected: defeats the cache (the prompt changes every call because the dynamic tail varies); produces near-zero hit rate.
4. **Static head + dynamic tail with explicit cache markers.** Chosen.

## Decision rationale

- **Order-of-magnitude cost reduction** for the most frequent agent calls. Linker on file-change events is the dominant cost driver; static head is ~80% of its prompt; cache hits typically exceed 70% over a 5-minute window of activity.
- **Forcing function for cleaner prompts.** Designing the head/tail split improves prompt clarity (instructions go up top; data goes at the bottom). Cache-friendliness aligns with prompt-engineering best practice.
- **Provider-portable.** The same structure works across Anthropic (explicit markers), OpenAI (automatic prefix caching), and local Ollama (no caching, but the prompt still works).
- **Measurable.** Cache hit ratio is a clean metric per agent; deviations surface concrete optimizations.

## Consequences

**Positive:**

- **Substantial cost reduction** at the dominant input-volume layer.
- **Cleaner prompts** by structural enforcement of "instructions first, data last."
- **Provider abstraction stays clean.** Each provider implementation knows how to express caching in its native form; agents are unaware.
- **Per-agent cache-hit-rate tracking** surfaces tuning opportunities (an agent whose dynamic-tail content keeps creeping into the head can be flagged).

**Negative:**

- **Prompt files require the `<!-- /cache -->` marker convention.** Agent authors must understand it. Mitigation: the agent spec template (12-agent-spec-template.md) shows the marker; prompt-evolution candidate variants inherit the marker structure.
- **Biographer/Voice-Keeper model updates invalidate cached heads** for every agent that includes them. Mitigation: monthly cadence is fine; the cache misses for the first call after each update are bounded.
- **Some agents have very small static heads** (e.g., Witness, which deliberately has no memory). For these, caching saves little. That's fine; the structure still works, just doesn't help much. Witness's cost is negligible regardless.
- **Provider-side caching is ephemeral (5min for Anthropic).** Bursty workloads benefit; sparse workloads don't. Mitigation: periodic agent runs (Cartographer hourly, Inquirer daily-reactive) cluster naturally; bursty file-change events naturally cluster; both benefit. Sparse on-demand calls (Untangler, Research Council) don't benefit much, but their per-call cost is tolerated by their value.

## References

- [`01-agents-and-council.md`](../01-agents-and-council.md) — agent spec template; cost-aware planning section
- [`12-agent-spec-template.md`](../12-agent-spec-template.md) — prompt skeleton format with implicit head/tail structure
- [`03-architecture.md`](../03-architecture.md) — `engram-llm` crate description, `token_estimator_calibration` table, performance budgets
- [`10-performance-budgets.md`](../10-performance-budgets.md) — cost budgets that depend on cache hits
- [Anthropic Prompt Caching docs](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching) — provider feature reference
