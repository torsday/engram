//! End-to-end integration test for the Devil's Advocate agent (#50).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/devils-advocate/{prompt.md, config.toml}` files (copied from the
//! repo root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Devil's Advocate behavior:
//!
//! 1. The config parses against `AgentConfig`, projecting to the `Editorial`
//!    ceiling and the documented 0.90 auto-land floor.
//! 2. The `prompt.md` loads through `prompt_loader` (cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` and include the
//!    well-defended decline case (the ADR 0007 restraint).
//! 4. A `DevilsAdvocateOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id`, and
//!    `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created (ADR 0003).
//!
//! Covers both output shapes: a defensible critique (claims + assumptions +
//! annotations) and a decline (well-defended note → no payload). The
//! Steelman-gate flow (#35) and council routing are a follow-up runner slice
//! — the same scope boundary at which the sibling agents (#52, #61, #49, #51,
//! #58) landed.
//!
//! See `docs/design/01-agents-and-council.md` §Devil's Advocate and
//! `crates/engram-agents/src/agents/devils_advocate.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::devils_advocate::DevilsAdvocateOutput;
use engram_agents::invasiveness::Invasiveness;
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
                input_tokens_total: 2_600,
                output_tokens: 1_100,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 2.6,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 3.0,
                total_cents: 5.6,
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
/// (with `agents/devils-advocate/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/devils-advocate/{prompt.md, config.toml}` from the checked-in
/// repo into the test vault's `agents/` directory so the runner can load them.
fn install_devils_advocate(vault: &Path) {
    let src = repo_root().join("agents").join("devils-advocate");
    let dst = vault.join("agents").join("devils-advocate");
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

/// The checked-in `agents/devils-advocate/config.toml` must parse against the
/// runner's `AgentConfig` and project to the Editorial ceiling. Devil's
/// Advocate's critique annotations land at the editorial level; anything
/// stronger routes through council.
#[test]
fn checked_in_devils_advocate_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/devils-advocate/config.toml"))
        .expect("config.toml must exist at agents/devils-advocate/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "devils-advocate");
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Editorial,
        "devils-advocate must cap at Editorial"
    );
    // 0.90 floor — between the constructive Steelman (0.85) and Voice Keeper
    // (0.92); a binding critique is higher-stakes than an additive annotation.
    assert!(
        (cfg.confidence_threshold - 0.90).abs() < 1e-6,
        "expected 0.90 auto-land floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_devils_advocate_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/devils-advocate/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("devils-advocate cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 devils-advocate eval cases, got {}",
        cases.len()
    );
    // The well-defended decline case must exist — it's the ADR 0007 restraint
    // (don't manufacture a contrarian critique that would fail the Steelman
    // gate). Dropping it would silently lose the agent's whole rationality bar.
    assert!(
        cases.iter().any(|c| c.id.contains("decline")),
        "devils-advocate cases must include the well-defended decline case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/devils-advocate/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_devils_advocate_prompt_loads() {
    let path = repo_root().join("agents/devils-advocate/prompt.md");
    let structured = engram_agents::prompt_loader::load(&path).expect("prompt must load");
    assert!(
        !structured.static_head.is_empty(),
        "static head must not be empty"
    );
    assert!(
        !structured.dynamic_tail.is_empty(),
        "dynamic tail must not be empty"
    );
    // The static head MUST include the role declaration.
    assert!(
        structured.static_head.contains("Devil's Advocate"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted critique output (central claim + unstated
/// assumption + annotation) round-trips through the runner. Asserts the
/// agent_runs row lands with the correlation_id and no-agent-commits holds.
#[tokio::test]
async fn devils_advocate_e2e_critique_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_devils_advocate(vault.path());

    // Shape-matches `DevilsAdvocateOutput` (mirrors the checked-in happy.json
    // fixture). The runner parses `confidence` / `rationale`; the payload rides
    // along. The round-trip below also asserts it parses back into the type.
    let response = r###"{
        "confidence": 0.84,
        "rationale": "The central claim assumes lossy compression preserves the most important features, but the cited examples are all cases where what counts as 'important' is itself the contested question.",
        "decline": false,
        "central_claims": [
            {
                "quote": "Editing is just lossy compression of intent.",
                "restated_claim": "Editing's value comes from dropping low-importance content while preserving high-importance content."
            }
        ],
        "unstated_assumptions": [
            {
                "assumption": "The editor and the original author agree on which content is high-importance.",
                "load_bearing": true,
                "why": "If they disagree, the editor is substituting their own intent, not compressing the original."
            }
        ],
        "proposed_annotations": [
            {
                "anchor_text": "lossy compression of intent",
                "insertion_context": "Editing is just lossy compression of intent.",
                "counter_note_ids": ["01H8X9", "01H8XA"],
                "critique": "01H8X9 explicitly argues editors and authors disagree on importance ranking; the analogy assumes agreement."
            }
        ],
        "standalone_critique": null
    }"###;

    let parsed: DevilsAdvocateOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.84).abs() < 1e-6);
    assert!(!parsed.decline, "a grounded critique must not decline");
    assert_eq!(parsed.central_claims.len(), 1);
    assert!(
        parsed.unstated_assumptions.iter().any(|a| a.load_bearing),
        "the critique surfaces a load-bearing assumption"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent(
            "devils-advocate",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("run_agent must succeed");

    assert!(
        !matches!(report.outcome, RunOutcome::Errored | RunOutcome::Panicked),
        "run must complete cleanly; got {:?}",
        report.outcome
    );
    assert!(!report.correlation_id.is_empty(), "correlation_id required");
    assert_ne!(
        report.correlation_id, report.run_id,
        "correlation_id must be distinct from run_id"
    );

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
    assert_eq!(row_agent, "devils-advocate");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted decline output (well-defended note → no critique payload)
/// round-trips through the runner. Covers the ADR 0007 restraint shape — a
/// decline carries no claims, assumptions, or annotations.
#[tokio::test]
async fn devils_advocate_e2e_decline_round_trips_without_payload() {
    let (sqlite, vault) = setup();
    install_devils_advocate(vault.path());

    let response = r###"{
        "confidence": 0.0,
        "rationale": "The note's central claim is well-defended: 01H8X9 supplies the counter-example the prompt would attack, the note acknowledges it, and the remaining assumptions are explicit and load-bearing. No defensible critique exists; manufacturing one would fail the Steelman gate.",
        "decline": true
    }"###;

    let parsed: DevilsAdvocateOutput = serde_json::from_str(response).expect("typed parse");
    assert!(parsed.decline, "a well-defended note must decline");
    assert!(
        parsed.central_claims.is_empty()
            && parsed.unstated_assumptions.is_empty()
            && parsed.proposed_annotations.is_empty(),
        "a decline carries no critique payload"
    );
    assert!(parsed.standalone_critique.is_none());

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent(
            "devils-advocate",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("run_agent must succeed on a decline response");

    assert!(!report.correlation_id.is_empty());
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'devils-advocate'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "decline run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
