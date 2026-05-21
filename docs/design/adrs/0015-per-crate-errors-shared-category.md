# ADR 0015: Per-crate Error enums + shared ErrorCategory

**Status:** Accepted

**Date:** 2026-05 (closes the design question left over from [#22](https://github.com/torsday/engram/issues/22) — the retry / circuit-breaker / timeout layer)

## Context

The retry-with-jitter + circuit-breaker work in #22 added a cross-crate
classification taxonomy at `engram_core::error::ErrorCategory` (four
variants: `Transient` / `Permanent` / `System` / `External`). Each
crate's concrete `Error` exposes `fn category() -> ErrorCategory` so the
resilience layer can decide retry vs. fail-fast without inspecting
variant-specific details.

#22's original AC asked for something stronger: a unified
`EngramError` mega-enum in `engram-core::error` with sub-variants like
`ProviderRateLimit`, `ProviderServerError`, `ContextOverflow`,
`ProviderAuthFailed`. That part was deliberately deferred — what
actually unblocked the resilience layer was the shared _category_, not a
unified enum — leaving an open design question recorded as
[#172](https://github.com/torsday/engram/issues/172).

This ADR resolves that question.

The current error topology, after the work in #15 / #19 / #22:

| Crate            | Concrete `Error`                                                                                                                         |
| ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `engram-llm`     | `Secrets`, `Http`, `Status`, `Decode`, `EmptyResponse`, `UnsupportedByProvider`, `Timeout`, `RetryBudgetExhausted`, `CircuitBreakerOpen` |
| `engram-secrets` | `NotFound`, `InvalidName`, `Unsupported`, `Backend`, `Audit`, `Io`                                                                       |
| `engram-git`     | `Open`, `RevParse`, `Status`, `Diff`, `Log`, `Object`, `Commit`, `Io`                                                                    |
| `engram-core`    | (no concrete `Error` today — `ErrorCategory` only)                                                                                       |

Every concrete variant is **domain-specific**: a keychain `Backend` error
only ever originates from the secrets crate; a git `RevParse` failure
only ever comes from `engram-git`. There is no current call site that
benefits from matching on a sibling crate's variant without going
through `category()`.

## Decision

**Keep the per-crate `Error` enums + shared `ErrorCategory` pattern. Do
not consolidate into a `EngramError` mega-enum.**

When a caller needs to make a fine-grained decision **inside the same
crate's domain** (e.g. "was this an Anthropic 429 specifically?"), the
owning crate exposes a sub-classifier next to its `Error`:

- `engram-llm` already has `Error::Status { status, message, retry_after_secs }`
  with a numeric status code. Callers that need "is this a 429?" call
  `matches!(err, Error::Status { status: 429, .. })`. No mega-enum
  variant required.
- Future per-domain refinements (e.g. an `LlmErrorKind` enum returned by
  a helper method on `engram_llm::Error`) belong in the owning crate,
  not in `engram-core`.

When a caller needs a **cross-crate** retry / breaker decision,
`error.category()` is the canonical answer.

### What stays where

- `engram_core::error::ErrorCategory` — the four-variant cross-crate
  vocabulary. Shared.
- `engram_core::error::*` — additional cross-cutting types iff they're
  genuinely cross-crate. Today there's nothing else.
- `<crate>::Error` — concrete, domain-specific variants. Owned by the
  crate. Implements `fn category(&self) -> ErrorCategory`.
- Per-domain sub-classifiers (e.g. `LlmErrorKind`) live in the owning
  crate when they prove useful.

### What does **not** happen

- No `engram_core::EngramError` enum.
- No migration of `engram-llm::Error::Status` into
  `engram_core::EngramError::ProviderServerError`.
- No requirement that every crate-error variant be representable in a
  central enum.

## Alternatives

### Alt 1 — Unified `EngramError` mega-enum

Consolidate every crate's variants into one enum in `engram-core::error`:

```rust
pub enum EngramError {
    ProviderRateLimit { provider: &'static str, retry_after_secs: Option<u64> },
    ProviderServerError { provider: &'static str, status: u16, message: String },
    ProviderAuthFailed { provider: &'static str, message: String },
    ContextOverflow { tokens: u32, model: String },
    Secrets(SecretsKind),
    Git(GitKind),
    // … per-domain sub-enums for each crate's variants …
}
```

**Pro:**

- A single `match` arm can branch on `ContextOverflow` from any code
  path.
- Stack traces would normalize to one shape.
- Agent runtime can match on `EngramError::ContextOverflow` directly
  rather than relying on string-matching the inner message.

**Con:**

- Every crate now depends on `engram-core` for its concrete error type.
  Today only `engram-llm` depends on `engram-core::ErrorCategory`. Mega-enum
  forces fan-in across the workspace, hurting build times and creating
  new circular-dep risk surfaces.
- Many variants are produced by exactly one crate. A `KeychainBackend`
  variant in `engram-core::EngramError` is dead noise to every crate
  except `engram-secrets`.
- The "match on `ContextOverflow` directly" use case is solved by
  exposing a `fn is_context_overflow(&self) -> bool` helper on
  `engram_llm::Error`, without enum consolidation.
- Adding a new variant requires editing a different crate from the one
  where the failure mode lives. Future-contributor confusion.

### Alt 2 — `engram-llm::ErrorKind` only (partial consolidation)

Leave most crates with their own errors; consolidate only the
LLM-provider taxonomy (`ProviderRateLimit`, `ProviderServerError`,
`ContextOverflow`, `ProviderAuthFailed`) into a single enum that all LLM
providers map onto.

**Pro:**

- The LLM call path is where most cross-provider matching happens
  (retry policies, tier-escalation triggers). One shape there is high-value.
- Doesn't touch unrelated crates.

**Con:**

- This is partly what `Error::Status { status }` plus `Error::category()`
  already provides. The marginal value of adding `ErrorKind` over the
  current pattern is small.
- Pulls future "is this a context overflow?" classification into
  `engram-llm`, which is fine, but it doesn't actually require a new enum
  — a `fn is_context_overflow(&self) -> bool` on `engram_llm::Error` is
  enough.
- Defer until a concrete agent-runtime call site exists that genuinely
  needs `ErrorKind`. YAGNI for now.

## Consequences

- **Documentation update:** `crates/engram-core/src/error.rs` carries a
  comment pointing back to this ADR so future contributors find the
  decision before re-litigating.
- **No code migration today.** The crates keep the shape they have.
- **Future refinements are possible without revisiting this ADR.** A
  per-crate `ErrorKind` sub-enum (e.g. on `engram-llm`) does not
  contradict this decision; it implements the "per-domain sub-classifier
  lives in the owning crate" pattern. File a new issue when a call site
  actually needs it.
- **Cross-crate `category()`** stays the canonical retry / breaker
  vocabulary. New crates that ship public errors implement `category()`.

## References

- [#22](https://github.com/torsday/engram/issues/22) — retry + breaker + timeout work that introduced `ErrorCategory`
- [#172](https://github.com/torsday/engram/issues/172) — design question this ADR closes
- `crates/engram-core/src/error.rs` — `ErrorCategory` definition
- `crates/engram-llm/src/error.rs` — `Error::category()` and `Error::Status` reference impl
- `docs/design/03-architecture.md` §Error handling and resilience — the retry / breaker spec
