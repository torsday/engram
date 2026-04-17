# ADR 0007: Steelman is a mandatory gate for all critical agents

**Status:** Accepted

**Date:** 2026-04 (added when the user observed that Heretic risked producing contrarian-for-its-own-sake critique)

## Context

Engram has critical agents whose job is to argue against, challenge, or stress-test the user's notes: Devil's Advocate, Heretic, Socratic Prober. Without quality control, an LLM playing these roles can easily produce sloppy disagreement: strawmen, weak counter-arguments, contrarianism that no thoughtful person would actually hold.

Sloppy critique is worse than no critique. It trains the user to dismiss criticism as noise; over time, the system's voice-of-disagreement becomes background static. The Maggie Appleton frame --- LLMs as epistemic rubber ducks, not oracles --- only works if the duck is sharp.

## Decision

**Steelman serves a dual role: constructive (strengthen weak notes) AND gate (review all critique before it lands).**

Every critical agent's output passes through Steelman before it can land. Steelman applies five criteria; **all** must hold:

1. **Engages the actual claim** (not a strawman simplification).
2. **Uses real evidence** (vault citation or verifiable external source, not assertion).
3. **Internally consistent** (the counter-position is a coherent alternative, not just negation).
4. **Has real-world adherents** (a thinker the user would respect could plausibly hold this view).
5. **Concedes what's true** (acknowledges what the original got right before challenging it).

If all five hold, the critique lands. If not, two outcomes:

- **Returned for revision** (Steelman explains which criteria failed; one revision attempt allowed).
- **Shelved with explicit "no defensible critique found" note** (this is itself useful information --- the original is robust).

The gate is structural; it cannot be bypassed by trust score or invasiveness ceiling.

## Alternatives considered

1. **Trust agents to self-police via prompt engineering.** "Don't be contrarian for its own sake." Rejected: prompts drift; behavior under load varies; no enforcement.
2. **Human always reviews critique.** Adds friction to every annotation; defeats agent value.
3. **Separate "Quality Reviewer" agent** instead of reusing Steelman. Plausible but adds a fourth critical-agent role; Steelman naturally inverts (constructive-strengthening is the same epistemic skill applied to "would this critique itself stand up?").
4. **Steelman as mandatory gate.** Chosen.

## Consequences

**Positive:**

- **Critique becomes high-signal.** When Devil's Advocate or Heretic raises an objection, the user takes it seriously --- the gate has filtered out the noise.
- **"No defensible critique found" is itself a useful signal.** It tells the user a note is robust at this level. That's a real finding, not a non-event.
- **The five criteria operationalize "rationality."** Without them, "rational" is a vague aspiration. With them, it's testable.
- **The critical roster stays valuable.** Heretic produces sustained alternatives the user respects; Devil's Advocate's critiques carry weight.

**Negative:**

- **Higher cost per critique.** Every critical action triggers a Steelman gate call. Mitigation: Steelman uses the `fast` model tier for the gate role (the constructive role uses standard); the cost is a small multiple, not an order of magnitude.
- **Steelman becomes a bottleneck.** If Steelman fails, all critique stalls. Mitigation: Steelman's gate role is simpler than its constructive role (yes/no on five criteria with rationale) --- failure modes are easier to detect.
- **Risk of false negatives.** Steelman might reject a legitimate critique because the counter-position seems implausible to the gate. Mitigation: the "request revision" outcome with criterion-level explanation lets the critical agent strengthen the case; Auditor reviews shelved-with-no-defensible-critique outcomes for false-positive patterns.
- **Risk of mutual capture.** Steelman gates Devil's Advocate; if Steelman is itself wrong, both fail together. Mitigation: Auditor reads samples of both in its quarterly evaluation; sustained miscalibration triggers prompt revision.

## References

- `01-agents-and-council.md` --- "The rationality gate (for critical agents)" section
- ADR 0002 --- agents-as-data (Steelman's prompt is editable to refine the criteria over time)
