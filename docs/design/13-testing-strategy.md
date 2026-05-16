# Testing Strategy

## Purpose

Engram is a long-running multi-agent system that mutates user data. The cost of a regression is real: a Linker that starts proposing wrong links erodes user trust; a confidence calibrator that drifts undermines the autonomy model; a git-safety bug could mean an agent committed silently. Testing has to catch these classes of failure, not just hit lines.

This document defines engram's testing approach: what we test, how we test it, what we don't test, and the standards each test must meet.

The principle: **tests assert behavior, not implementation.** A refactor that preserves behavior must not break tests. A bug fix should be preceded by a test that reproduces the bug.

---

## Layers of testing

Engram uses five layers, each with a clear purpose. Coverage targets are at the layer level, not aggregate.

| Layer                       | Crate / location                       | Purpose                                            | Target |
| --------------------------- | -------------------------------------- | -------------------------------------------------- | ------ |
| **Unit (pure)**             | inline `#[cfg(test)] mod tests`        | Pure functions: parsing, classification, formulas. | ≥ 90% line |
| **Property-based**          | `tests/property/` per crate            | Invariants that must hold for *all* inputs.        | Per-property |
| **Snapshot**                | `tests/snapshots/` per crate           | Stable outputs of complex transformations.         | Per-snapshot |
| **Integration (in-process)**| `tests/integration/`                   | Multi-component flows using a real sqlite + temp vault. | Critical paths covered |
| **End-to-end (vault scenarios)** | `tests/e2e/`                      | Full `engram serve` against a real vault, with mocked LLM. | All v1 acceptance criteria |

Tests run via `cargo test --workspace` (unit + property + snapshot + integration) and `task e2e` (end-to-end). All run in CI.

---

## Unit tests (pure functions)

Test pure functions in isolation. No I/O, no side effects, no mocks needed.

**Targets:**

- `engram-core::frontmatter` — parsing, serialization, schema validation
- `engram-core::markdown` — AST construction, wikilink extraction, block-ID detection
- `engram-core::slug` — title-to-slug, collision-suffix logic
- `engram-core::ulid` — generation, parsing, sortability
- `engram-rubric` — every evergreen-rubric check
- `engram-agents::invasiveness` — the deterministic classifier
- `engram-agents::confidence_formula` — per-agent formulas
- `engram-llm::retry` — retry logic with mocked clock and mocked transient errors
- `engram-extract` — file-type classification heuristics
- `engram-index::search::rrf` — Reciprocal Rank Fusion math

**Standard:** every public function in these modules has at least one test for the happy path, one for an edge case (empty input, max-size input), and one for the error path. Coverage targets ≥ 90% lines and ≥ 80% branches per module.

**Anti-pattern:** **no tests that just call a function and assert non-`None`.** A test must assert *what* the result is, not just that it returned.

---

## Property-based tests

Engram has many invariants that should hold for *any* input. These are the natural fit for `proptest` (or `quickcheck`).

**Properties to verify:**

| Property | Invariant |
|---|---|
| Slug round-trip | `slug(title)` is deterministic; same title → same slug |
| Slug collision detection | After applying collision-suffix, all slugs in a vault are unique |
| ULID sortability | For two ULIDs `a` (generated at `t_a`) and `b` (generated at `t_b > t_a`), `a < b` as strings |
| Frontmatter round-trip | `parse(serialize(frontmatter)) == frontmatter` for any valid frontmatter |
| Sidecar round-trip | `parse(serialize(sidecar)) == sidecar` for any valid sidecar |
| Wikilink extraction | For markdown containing `[[X]]`, X appears in the extracted link list |
| Markdown AST round-trip | Modifying via the AST and rendering produces valid markdown |
| Hybrid retrieval ordering | RRF result ordering is consistent across reorderings of input rankings (modulo ties) |
| Invasiveness classifier | For any diff that adds-only, classification is `additive` or higher (never `mechanical`) |
| Confidence formula | `confidence_final ∈ [0.0, 1.0]` for any inputs in `[0.0, 1.0]` |
| FSRS update | A "good" review never decreases stability; an "again" review always decreases it |
| Idempotent capture | Submitting the same capture (same ULID) twice produces one `artifacts` row |

**Standard:** each property runs ≥ 1000 generated cases; failures are minimized via `proptest` shrinking to the smallest failing input.

**Anti-pattern:** **no property tests that just exercise serialization.** Properties must encode behavior the system *promises* — invariants the user or another component relies on.

---

## Snapshot tests

For complex transformations whose output is non-trivial but should be stable.

**Targets:**

- Agent prompt rendering (given a fixed context, the rendered prompt is byte-identical to a checked-in snapshot)
- Standup report rendering (given a fixed system state, the standup is identical)
- Annual Review structure (given a fixed sample vault year, the review structure is identical)
- Council deliberation transcript format
- Sidecar JSON format (formatting/key order/spacing)
- API response bodies for stable endpoints (`/biography`, `/calibration`, `/standup`)
- Migration outputs (given an N-1 schema, applying the migration produces the N schema byte-identically)

**Tool:** `insta` for inline snapshots; reviewer must approve any change to a snapshot via `cargo insta review`.

**Standard:** snapshot changes show up in code review. A snapshot diff requires explicit approval; nobody hits "accept all" without reading.

**Anti-pattern:** **no snapshot tests of LLM outputs.** LLM responses are non-deterministic; snapshotting them produces flaky tests. Mock the LLM for snapshot tests.

---

## Integration tests

Multi-component tests that wire real sqlite, a real temp vault directory, and real file-watcher events. The LLM is mocked.

**Standard fixture setup:**

```rust
let fixture = TestFixture::new()
    .with_temp_vault()                  // temp directory + git init
    .with_sqlite_index()                // real index.sqlite
    .with_seeded_notes(SEED_VAULT)      // 50-note baseline
    .with_mock_llm(MockLLM::new()       // deterministic responses
        .respond("linker", LINKER_RESP)
        .respond("scribe", SCRIBE_RESP))
    .build()?;
```

**Critical paths covered:**

| Test | What it verifies |
|---|---|
| `test_ingest_pdf_to_literature_note` | Drop a PDF; literature note appears in proposals; user approves; lands unstaged. |
| `test_linker_proposes_high_confidence` | New note with strong neighbor; Linker auto-lands; `agent_actions` row created; markdown updated. |
| `test_linker_proposes_low_confidence` | New note with weak neighbor; Linker enters proposal queue; no markdown change. |
| `test_no_agent_commits_invariant` | Run all v1 agents against a sample vault for 10 simulated minutes. Assert: `git log --all` shows no commits authored by anything other than the test harness. |
| `test_concurrent_agents_advisory_lock` | Two agents attempt to modify the same note. One wins the lock; the other defers. Both runs are logged correctly. |
| `test_confidence_calibration_loop` | Run Linker 100 times; reject 30; verify Watcher's calibration record reflects 70% acceptance and proposes a prompt-tuning variant if rate drops below threshold. |
| `test_atomic_triple_write_crash_recovery` | Begin a write; SIGKILL the process between markdown and sidecar writes; restart; verify recovery via the `write_intents` log; final state consistent. |
| `test_git_restore_re_indexes` | Stage and discard an agent change via `git restore`; verify indexer re-reads the file, updates `notes_fts` (SQLite) and queues a LanceDB upsert to reflect the restored content. |
| `test_lancedb_eventual_consistency_window` | Modify a note; query semantic search immediately (should still return correct results via BM25 + graph fallback); query again after 1s (LanceDB now reflects the new content). |
| `test_lancedb_reconciliation_after_crash` | Modify a note; SIGKILL the process before async LanceDB upsert completes; restart; verify reconciliation pass detects the mismatch and re-upserts. |
| `test_capture_idempotency` | Submit the same capture twice (same ULID); verify only one artifact and one literature note. |
| `test_offline_capture_sync` | Simulate Swift app submitting captures while server is "down"; bring server back up; verify all captures sync once. |
| `test_pacekeeper_throttle_state_transitions` | Force backlog growth; verify Pacekeeper transitions normal → throttled → paused; verify deferred agents stop running. |
| `test_cost_cap_pause` | Inject usage that crosses 100% cap; verify all LLM-using agents pause; verify mechanical agents continue; verify cap warning surfaces. |
| `test_sub_agent_invocation` | Curator invokes Synthesizer; verify lock inheritance, separate memory namespaces, parent_run_id linkage in `agent_actions`. |
| `test_external_mcp_consent_flow` | Submit `POST /mcp/register`; assert pending; user approves via mock Swift channel; assert client receives API key once. |
| `test_external_mcp_scope_enforcement` | Client with `notes:read:tag/travel` attempts to read a note tagged `topic/work`; assert 403. |
| `test_ask_user_round_trip` | Client calls `ask_user`; mock user replies via test harness; client polls and receives the answer. |
| `test_privacy_zone_blocks_cloud_llm` | Note in `notes/work/`; verify any agent processing it uses the local provider regardless of agent config. |
| `test_schema_migration_old_vault` | Apply migrations from N-1 → N to a vault snapshot; verify no data loss; verify functional after migration. |

**Standard:** every test in this list is a real test that catches a real regression. Any test that's failing intermittently is fixed or removed. Every commit that fixes a bug must include the failing test that reproduces it.

**Anti-pattern:** **no tests that mock everything.** If the test mocks the database AND the file system AND the agents AND the git layer, it's testing nothing.

---

## End-to-end vault scenarios

Run the actual `engram serve` binary against a real vault on a temp port. Drive it via the REST API. The LLM is mocked at the HTTP layer (proxy intercepts and returns scripted responses).

**Standard fixture:** `e2e/fixtures/realistic-vault/` — a 200-note vault representing a plausible early-stage user state, with seeded `agent_actions`, `predictions`, sidecars, etc.

**Scenarios covered (one per scenario in `11-scenarios.md`):**

- Morning routine: standup → diff queue → review → commit
- Capture in the wild: offline queue → sync → diff appears
- A hard problem: Untangler + Research Council → briefing note (Untangler / Research Council land in v2.2; the e2e scenario is exercised then)
- Year-end ritual: Annual Review renders (in v1)
- First time: greenfield wizard end-to-end
- Migration: corpus digestion of a small fixture corpus
- A bad agent moment: Heretic produces output → user discards with reason → calibration updates
- The stuck-for-a-week pattern: Pacekeeper transitions verified

Each scenario has a pass/fail outcome and runs to completion in < 60s wall-clock with mocked LLM.

**Standard:** **every numbered scenario in `11-scenarios.md` has a corresponding e2e test.** A scenario that can't be tested e2e indicates either a missing test infrastructure piece or a scenario that needs revision.

---

## Mock LLM provider

A central piece of test infrastructure. The mock LLM is an HTTP server that:

- Listens on `127.0.0.1:<port>`
- Accepts requests in the same shape as Anthropic / OpenAI APIs
- Returns scripted responses based on a per-test response map
- Supports configurable error injection (return 429, 5xx, slow response, malformed JSON)
- Records every call for assertion (was it called? with what prompt? how many times?)

```rust
let mock = MockLLM::new()
    .respond_to_agent("linker", json!({
        "confidence": 0.92,
        "rationale": "Strong semantic agreement.",
        "proposed_links": [...]
    }))
    .respond_to_agent("scribe", LinkerOverflow)  // 429 rate limit
    .respond_to_agent("synthesizer", SlowResponse(Duration::from_secs(70))) // timeout test
    .start();
```

The mock is used by integration tests AND e2e tests (run as a sidecar process for e2e). Real LLM calls are explicitly opt-in via `ENGRAM_REAL_LLM=1` env var, run only in nightly CI to detect provider drift.

---

## Coverage targets

Per layer (not aggregate; aggregate coverage % is a vanity metric):

| Layer | Target |
|---|---|
| Unit (pure) | ≥ 90% line, ≥ 80% branch in core modules |
| Property | ≥ 1000 cases per property; all critical invariants property-tested |
| Snapshot | All listed snapshot targets have current snapshots |
| Integration | All listed critical paths covered; new flows added when the system gains new capabilities |
| E2E | One scenario per numbered scenario in `11-scenarios.md` |

CI fails if:
- Any test fails
- Any test is `#[ignore]`'d without a tracked GitHub issue justifying it
- Any property test reduces its case count below 1000 without justification
- Coverage drops below targets in core modules (per `tarpaulin` or `llvm-cov`)

---

## What we deliberately don't test

- **LLM output quality.** Real LLM responses vary; testing "did the LLM produce a good response" is a moving target. We test that *the system handles the LLM's response correctly* (parses, validates, applies, retries on error). LLM quality is a Watcher/Auditor concern at runtime, not a unit-test concern.
- **Real provider integration.** Tests don't hit Anthropic or OpenAI. The mock LLM is the contract; if the contract drifts (provider changes API), nightly real-LLM tests catch it.
- **UI/visual regression of Swift app.** Out of scope for v1 testing infrastructure; manual TestFlight is sufficient.
- **Performance of full retrieval against a 100K-note vault.** v1 scale ceiling is 10K notes; we test at that scale. Larger-scale benchmarks in v3+.
- **Adversarial security testing.** Threat model (`09-threat-model.md`) names defenses. v1 doesn't include red-team scope; documented out of scope.

---

## Testing the no-agent-commit invariant (specifically called out)

This invariant is so important it gets its own test. Two tests, in fact:

### `test_no_agent_commits_invariant` (integration)

```rust
#[test]
fn test_no_agent_commits_invariant() {
    let fixture = TestFixture::new()
        .with_seeded_notes(SEED_VAULT)
        .with_all_v1_agents_enabled()
        .with_mock_llm(realistic_responses())
        .build();

    // Run the system for 10 simulated minutes; trigger many file events.
    fixture.tick_simulated_minutes(10);
    fixture.flush_pending_work();

    // Assert: no commits exist beyond the initial seed commit.
    let commits = fixture.git_log_all();
    assert_eq!(commits.len(), 1, "expected only the seed commit; agents committed:\n{}",
        commits.iter().skip(1).map(|c| format!("  {} by {}", c.sha, c.author)).join("\n"));
}
```

### `test_write_git_handle_compile_check`

A compile-fail test (using `trybuild`) that asserts: agent code attempting to call `WriteGit` methods fails to compile. This verifies the type-level enforcement from ADR 0009.

```rust
// tests/compile-fail/agent_cannot_commit.rs
fn agent_main(git: impl ReadOnlyGit) {
    git.commit("oops", CommitOpts::default()); //~ ERROR no method named `commit`
}
```

`trybuild` runs this and asserts the compiler emits the expected error.

---

## How tests evolve

When a bug is reported:
1. Write a failing test that reproduces it.
2. Fix the code.
3. The test is now part of the regression suite.

When a feature is added:
1. Write the integration test for the happy path before implementation.
2. Implement.
3. Add property/snapshot tests for invariants the feature introduces.
4. If user-facing, add an e2e scenario test.

Test code is reviewed with the same rigor as production code. Sloppy tests rot the suite; rotting tests get ignored; ignored tests catch nothing.

---

## CI integration

```
.github/workflows/ci.yml:
  - cargo fmt --check
  - cargo clippy -- -D warnings
  - cargo test --workspace          # unit, property, snapshot, integration
  - task e2e                         # end-to-end with mock LLM
  - cargo audit                      # dependency vulnerability scan
  - cargo deny check                 # license + banned-crates policy
  - task format-check                # prettier on docs

nightly:
  - ENGRAM_REAL_LLM=1 cargo test --test real_provider_drift
  - cargo cyclonedx --output-format json --output-file sbom.json
```

PRs cannot merge with red CI. Nightly failures open issues automatically.
