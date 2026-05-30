//! End-to-end integration test for the Annual Review agent (#61).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/annual-review/{prompt.md, config.toml}` files (copied from the repo
//! root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Annual Review behavior:
//!
//! 1. The Annual Review's `config.toml` parses against `AgentConfig` without
//!    error (catches regressions to either schema), with a ≥ 1-year cron
//!    period.
//! 2. The `prompt.md` loads through `prompt_loader` (catches missing or
//!    malformed cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` (catches YAML breakage
//!    before a scorecard run) and include the maturity-abstention case.
//! 4. An `AnnualReviewOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id` returned on
//!    `RunReport`, and `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created, no commit or stage
//!    operation is performed. The runner's contract from ADR 0003 is honoured
//!    for the temporal-agent path too.
//!
//! See `docs/design/01-agents-and-council.md` §Annual Review for the agent's
//! spec and `crates/engram-agents/src/agents/annual_review.rs` for the Rust
//! types.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::annual_review::AnnualReviewOutput;
use engram_agents::locks::{LockConfig, LockManager};
use engram_agents::runner::{AgentConfig, AgentRunner, RunOutcome, TriggerContext};
use engram_index::sqlite::Migrator;
use engram_llm::{
    CompleteOptions, Completion, Cost, EmbeddingModel, LlmProvider, Model, ModelProvider,
    PromptStructured, StreamedCompletion, Usage,
};
use rusqlite::Connection;
use tempfile::tempdir;

/// Scripted provider — returns queued responses in order. One use, no API
/// surface; keep private to this test file.
struct Scripted {
    responses: StdMutex<Vec<String>>,
}

#[async_trait]
impl LlmProvider for Scripted {
    async fn complete(
        &self,
        _prompt: &PromptStructured,
        model: &Model,
        _options: &CompleteOptions,
    ) -> engram_llm::Result<Completion> {
        let text = {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(engram_llm::Error::Decode("script exhausted".into()));
            }
            q.remove(0)
        };
        Ok(Completion {
            text,
            usage: Usage {
                input_tokens_total: 4_000,
                output_tokens: 2_500,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 4.0,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 6.0,
                total_cents: 10.0,
            },
            model_used: format!("mock/{}", model.name),
            latency_ms: 5,
        })
    }

    async fn complete_streamed(
        &self,
        _: &PromptStructured,
        _: &Model,
        _: &CompleteOptions,
    ) -> engram_llm::Result<StreamedCompletion> {
        unreachable!("integration test never streams")
    }

    async fn embed(&self, _: &str, _: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
        unreachable!("integration test never embeds")
    }
}

/// Locate the repository root from the test's `CARGO_MANIFEST_DIR`. The crate's
/// manifest dir is `<repo-root>/crates/engram-agents`; the vault we care about
/// (with `agents/annual-review/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/annual-review/{prompt.md, config.toml}` from the checked-in
/// repo into the test vault's `agents/` directory so the runner can load them.
/// Done as a copy rather than a symlink so the test is hermetic and the
/// integration covers the disk-load path.
fn install_annual_review(vault: &Path) {
    let src = repo_root().join("agents").join("annual-review");
    let dst = vault.join("agents").join("annual-review");
    std::fs::create_dir_all(&dst).unwrap();
    for file in ["prompt.md", "config.toml"] {
        std::fs::copy(src.join(file), dst.join(file))
            .unwrap_or_else(|e| panic!("copy {file}: {e}"));
    }
}

fn setup() -> (Arc<Mutex<Connection>>, tempfile::TempDir) {
    let tmp = tempdir().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    Migrator::new(&conn).apply_all().unwrap();
    (Arc::new(Mutex::new(conn)), tmp)
}

fn make_runner(
    sqlite: &Arc<Mutex<Connection>>,
    provider: Arc<dyn LlmProvider>,
    vault: &Path,
) -> AgentRunner {
    AgentRunner::new(
        Arc::clone(sqlite),
        provider,
        Model {
            provider: ModelProvider::Anthropic,
            name: "test".into(),
        },
        vault.join("agents"),
        LockManager::new(
            Arc::clone(sqlite),
            LockConfig {
                ttl_secs: 60,
                max_retries: 2,
                retry_base_ms: 5,
            },
        ),
        vault.to_path_buf(),
    )
}

/// The checked-in `agents/annual-review/config.toml` must parse against the
/// runner's `AgentConfig` schema. A drift would manifest as a hard-to-debug
/// runtime failure in production; surface it here.
#[test]
fn checked_in_annual_review_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/annual-review/config.toml"))
        .expect("config.toml must exist at agents/annual-review/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "annual-review");
    // Trigger is cron with a yearly period (365 days = 31_536_000s).
    assert!(
        cfg.cron_interval_secs >= 31_536_000,
        "annual-review must use a ≥1-year cron period, got {}s",
        cfg.cron_interval_secs
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_annual_review_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/annual-review/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("annual-review cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft and
    // not worth asserting here.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 annual-review eval cases, got {}",
        cases.len()
    );
    // The maturity-abstention case must exist — it's the one whose expected
    // behavior tests the deterministic gate, and removing it would silently
    // lose coverage of the safety path.
    assert!(
        cases.iter().any(|c| c.id.contains("abstain")),
        "annual-review cases must include a maturity-abstention case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/annual-review/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_annual_review_prompt_loads() {
    let path = repo_root().join("agents/annual-review/prompt.md");
    let structured = engram_agents::prompt_loader::load(&path).expect("prompt must load");
    assert!(
        !structured.static_head.is_empty(),
        "static head must not be empty"
    );
    assert!(
        !structured.dynamic_tail.is_empty(),
        "dynamic tail must not be empty"
    );
    // The static head MUST include the role declaration; otherwise we've
    // accidentally put the marker before everything important.
    assert!(
        structured.static_head.contains("Annual Review"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable;
    // that's the whole point of the head/tail split.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: scripted Annual Review output round-trips through the
/// runner. Asserts the agent_runs row lands with the correlation_id and the
/// no-agent-commits invariant holds.
#[tokio::test]
async fn annual_review_e2e_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_annual_review(vault.path());

    // Shape-matches `AnnualReviewOutput`. The runner only parses `confidence` /
    // `rationale` from this in the current slice; everything else is along for
    // the ride. The round-trip below also asserts the JSON parses back into our
    // typed `AnnualReviewOutput`.
    let response = r###"{
        "confidence": 0.89,
        "rationale": "Sustained activity across 14 months; three themes corroborated; milestones clear from git log.",
        "maturity_gate": false,
        "year": 2026,
        "output_path": "reflections/annual/2026.md",
        "themes": ["legibility", "attention", "self-rewriting tools"],
        "milestones": ["shipped engram v1", "abandoned the chat UI thread"],
        "narrative": "## 2026\n\nThe year the vault learned to rewrite itself."
    }"###;

    // Sanity: the response parses into our typed output. If `AnnualReviewOutput`
    // ever drifts away from this canonical shape the assertion fires before we
    // run the agent.
    let parsed: AnnualReviewOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.89).abs() < 1e-6);
    assert!(!parsed.maturity_gate);
    assert_eq!(parsed.year, 2026);
    assert_eq!(parsed.output_path, "reflections/annual/2026.md");
    assert_eq!(parsed.themes.len(), 3);

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("annual-review", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed");

    // Outcome assertions — in the current runner slice, an output without
    // `proposed_changes` and with confidence ≥ threshold produces a clean
    // decision. Annual Review's always-propose policy is enforced by the
    // future decision-matrix slice (it reads `[annual-review].always_propose`);
    // the relevant invariant for *this* slice is that the run was recorded.
    assert!(
        matches!(report.outcome, RunOutcome::NoAction | RunOutcome::AutoLand),
        "outcome must be a clean decision; got {:?}",
        report.outcome
    );

    // Correlation ID invariant — non-empty ULID distinct from run_id.
    assert!(!report.correlation_id.is_empty(), "correlation_id required");
    assert_ne!(
        report.correlation_id, report.run_id,
        "correlation_id must be distinct from run_id"
    );

    // The `agent_runs` row must persist with the same run_id and
    // correlation_id. The downstream sub-agent chain / tracing collector
    // depends on this being the join key.
    let (row_run, row_corr, row_agent, row_outcome): (String, String, String, String) = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT id, correlation_id, agent_name, outcome FROM agent_runs WHERE id = ?1",
            [&report.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("agent_runs row must exist")
    };
    assert_eq!(row_run, report.run_id);
    assert_eq!(row_corr, report.correlation_id);
    assert_eq!(row_agent, "annual-review");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    // No-agent-commits invariant (ADR 0003). The vault's working-tree root has
    // *no* `.git/` directory because we didn't initialize one; a regression
    // that tried to call `git add` would either succeed outside the vault
    // (catastrophic — surfaces as a parent-dir write) or fail. We assert
    // neither happened by checking nothing was written under `.git/`.
    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// Maturity-gate stub round-trips through the runner just like a real
/// reflection. The pre-LLM short-circuit lives in the future decision-matrix
/// slice (it'll skip the LLM call entirely when the gate trips); for now we
/// verify the *response shape* is parseable so that future slice has a clean
/// target.
#[tokio::test]
async fn annual_review_e2e_maturity_stub_response_round_trips() {
    let (sqlite, vault) = setup();
    install_annual_review(vault.path());

    // The host's `AnnualReviewOutput::maturity_stub` produces this shape. We
    // assert it (a) parses as `AnnualReviewOutput` and (b) flows through the
    // runner without issue.
    let stub = AnnualReviewOutput::maturity_stub(
        2026,
        "Vault too young for an annual review: 211 days since first note (365 required).",
    );
    let response = serde_json::to_string(&stub).expect("serialize stub");

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("annual-review", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed even on a stub response");

    assert!(!report.correlation_id.is_empty());
    // We don't constrain outcome here — the current runner doesn't yet know
    // about the gate; once the decision-matrix slice lands, this case becomes
    // "outcome == Proposal with maturity_gate: true".
    let _ = report.outcome;
}
