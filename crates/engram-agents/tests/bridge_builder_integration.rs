//! End-to-end integration test for the Bridge Builder agent (#65).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/bridge-builder/{prompt.md, config.toml}` files (copied from the repo
//! root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Bridge Builder behavior:
//!
//! 1. The config parses against `AgentConfig`, projecting to the `Editorial`
//!    ceiling and the documented 0.83 floor. (Bridge *links* auto-land at high
//!    confidence; bridge *notes* are council-routed.)
//! 2. The `prompt.md` loads through `prompt_loader` (cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` and include the
//!    all-meaningful restraint case.
//! 4. A `BridgeBuilderOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id`, and
//!    `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created (ADR 0003).
//!
//! Covers both output shapes: a mixed batch (accidental-link, accidental-note,
//! and meaningful verdicts) and an all-meaningful restraint scan (every pair
//! declined, no bridges). The link auto-land / note council-routing split is a
//! follow-up runner slice — the same scope boundary at which the sibling agents
//! landed.
//!
//! See `docs/design/01-agents-and-council.md` §Bridge Builder and
//! `crates/engram-agents/src/agents/bridge_builder.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::bridge_builder::{BridgeBuilderOutput, BridgeVerdict};
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
                input_tokens_total: 3_500,
                output_tokens: 1_500,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 3.5,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 4.5,
                total_cents: 8.0,
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
/// (with `agents/bridge-builder/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/bridge-builder/{prompt.md, config.toml}` from the checked-in
/// repo into the test vault's `agents/` directory so the runner can load them.
fn install_bridge_builder(vault: &Path) {
    let src = repo_root().join("agents").join("bridge-builder");
    let dst = vault.join("agents").join("bridge-builder");
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

/// The checked-in `agents/bridge-builder/config.toml` must parse against the
/// runner's `AgentConfig` and project to the Editorial ceiling — a bridge note
/// is an Editorial change routed through council, while a bridge link auto-lands
/// at high confidence.
#[test]
fn checked_in_bridge_builder_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/bridge-builder/config.toml"))
        .expect("config.toml must exist at agents/bridge-builder/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "bridge-builder");
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Editorial,
        "bridge-builder must cap at Editorial"
    );
    assert!(
        (cfg.confidence_threshold - 0.83).abs() < 1e-6,
        "expected 0.83 floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_bridge_builder_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/bridge-builder/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("bridge-builder cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 bridge-builder eval cases, got {}",
        cases.len()
    );
    // The all-meaningful restraint case must exist — it's the safety case
    // asserting Bridge Builder won't manufacture bridges between genuinely
    // unrelated clusters (the common, correct outcome in a healthy vault).
    assert!(
        cases.iter().any(|c| c.id.contains("all-meaningful")),
        "bridge-builder cases must include the all-meaningful restraint case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/bridge-builder/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_bridge_builder_prompt_loads() {
    let path = repo_root().join("agents/bridge-builder/prompt.md");
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
        structured.static_head.contains("Bridge Builder"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted mixed-batch output (accidental link + accidental
/// note + meaningful) round-trips through the runner. Asserts the agent_runs
/// row lands with the correlation_id and no-agent-commits holds.
#[tokio::test]
async fn bridge_builder_e2e_mixed_batch_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_bridge_builder(vault.path());

    // Shape-matches `BridgeBuilderOutput` (mirrors the checked-in happy.json
    // fixture). The runner parses `confidence` / `rationale`; the payload rides
    // along. The round-trip below also asserts it parses back into the type.
    let response = r###"{
        "confidence": 0.79,
        "rationale": "Three cluster pairs analyzed: two are meaningfully disconnected and one is an accidental gap (the rate-distortion cluster and the editing cluster both reach for lossy compression).",
        "cluster_pair_verdicts": [
            {
                "cluster_a_id": "c-rate-distortion",
                "cluster_b_id": "c-editing",
                "verdict": "accidental_link",
                "reasoning": "Both clusters reach for lossy compression; 01H8X9 has an explicit anchor that fits 01H8XA.",
                "proposed_bridge": {
                    "source_note_id": "01H8X9",
                    "target_note_id": "01H8XA",
                    "anchor_text": "rate-distortion",
                    "justification": "01H8X9's rate-distortion analogy directly informs 01H8XA's claim about editor choice."
                }
            },
            {
                "cluster_a_id": "c-woodworking",
                "cluster_b_id": "c-rust",
                "verdict": "meaningful",
                "reasoning": "Two genuinely unrelated topics; the author maintains them as separate projects.",
                "proposed_bridge": null
            },
            {
                "cluster_a_id": "c-systems-design",
                "cluster_b_id": "c-knowledge-systems",
                "verdict": "accidental_note",
                "reasoning": "Both clusters circle the same abstraction without any one note being the right place for a single link.",
                "proposed_bridge": {
                    "title": "Feedback loops in self-improving systems",
                    "slug": "feedback-loops-self-improving-systems",
                    "body": "Both software design and knowledge management depend on feedback loops between outputs and inputs.",
                    "cluster_a_anchor_note_ids": ["01H9A1", "01H9A2"],
                    "cluster_b_anchor_note_ids": ["01H9B1", "01H9B2"]
                }
            }
        ]
    }"###;

    let parsed: BridgeBuilderOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.79).abs() < 1e-6);
    assert_eq!(parsed.cluster_pair_verdicts.len(), 3);
    assert!(
        parsed
            .cluster_pair_verdicts
            .iter()
            .any(|v| v.verdict == BridgeVerdict::AccidentalLink && v.proposed_bridge.is_some()),
        "an accidental_link verdict must carry a proposed bridge"
    );
    assert!(
        parsed
            .cluster_pair_verdicts
            .iter()
            .any(|v| v.verdict == BridgeVerdict::Meaningful && v.proposed_bridge.is_none()),
        "a meaningful verdict carries no bridge"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("bridge-builder", TriggerContext::OnDemand { note_id: None })
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
    assert_eq!(row_agent, "bridge-builder");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted all-meaningful scan (every pair declined, no bridges) round-trips
/// through the runner. Covers the restraint shape — the common, correct outcome
/// in a healthy vault, where Bridge Builder proposes nothing.
#[tokio::test]
async fn bridge_builder_e2e_all_meaningful_round_trips_without_bridges() {
    let (sqlite, vault) = setup();
    install_bridge_builder(vault.path());

    let response = r###"{
        "confidence": 0.88,
        "rationale": "Healthy vault scan: every cluster pair examined is meaningfully disconnected. Defaulting to decline across all pairs is the right outcome.",
        "cluster_pair_verdicts": [
            {
                "cluster_a_id": "c-woodworking",
                "cluster_b_id": "c-rust",
                "verdict": "meaningful",
                "reasoning": "Two genuinely unrelated topics.",
                "proposed_bridge": null
            },
            {
                "cluster_a_id": "c-recipes",
                "cluster_b_id": "c-personal-journal",
                "verdict": "meaningful",
                "reasoning": "Distinct purposes; the few shared tokens are domain coincidences."
            }
        ]
    }"###;

    let parsed: BridgeBuilderOutput = serde_json::from_str(response).expect("typed parse");
    assert!(
        parsed
            .cluster_pair_verdicts
            .iter()
            .all(|v| v.verdict == BridgeVerdict::Meaningful),
        "a healthy-vault scan declines every pair"
    );
    assert!(
        parsed
            .cluster_pair_verdicts
            .iter()
            .all(|v| v.proposed_bridge.is_none()),
        "no bridges are proposed when every pair is meaningful"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("bridge-builder", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed on an all-meaningful scan");

    assert!(!report.correlation_id.is_empty());
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'bridge-builder'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "all-meaningful run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
