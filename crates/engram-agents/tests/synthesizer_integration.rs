//! End-to-end integration test for the Synthesizer agent (#49).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/synthesizer/{prompt.md, config.toml}` files (copied from the repo
//! root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Synthesizer behavior:
//!
//! 1. The Synthesizer's `config.toml` parses against `AgentConfig` without
//!    error, projecting to the `Structural` invasiveness ceiling (every
//!    output is council-routed, never auto-landed) and the documented 0.80
//!    discard floor.
//! 2. The `prompt.md` loads through `prompt_loader` (catches missing or
//!    malformed cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` (catches YAML
//!    breakage before a scorecard run) and include the decline safety case.
//! 4. A `SynthesizerOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id` returned on
//!    `RunReport`, and `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created, no commit or
//!    stage operation is performed (ADR 0003). Synthesizer proposes
//!    Structural changes through council — it must never touch git itself.
//!
//! Covers both output shapes: a coherent cluster (propose an evergreen) and
//! an incoherent one (decline, no payload). The council-routing of the
//! Structural proposal is a follow-up runner slice — the same scope boundary
//! at which the sibling agents (#52 Inquirer, #61 Annual Review) landed.
//!
//! See `docs/design/01-agents-and-council.md` §Synthesizer for the agent's
//! spec and `crates/engram-agents/src/agents/synthesizer.rs` for the Rust
//! types.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::synthesizer::SynthesizerOutput;
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
                input_tokens_total: 3_000,
                output_tokens: 1_200,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 3.0,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 3.5,
                total_cents: 6.5,
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
/// (with `agents/synthesizer/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/synthesizer/{prompt.md, config.toml}` from the checked-in repo
/// into the test vault's `agents/` directory so the runner can load them. Done
/// as a copy rather than a symlink so the test is hermetic and the integration
/// covers the disk-load path.
fn install_synthesizer(vault: &Path) {
    let src = repo_root().join("agents").join("synthesizer");
    let dst = vault.join("agents").join("synthesizer");
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

/// The checked-in `agents/synthesizer/config.toml` must parse against the
/// runner's `AgentConfig` schema and project to the Structural ceiling. A
/// regression that weakened the ceiling would let evergreen creation auto-land
/// instead of routing through council — a safety violation.
#[test]
fn checked_in_synthesizer_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/synthesizer/config.toml"))
        .expect("config.toml must exist at agents/synthesizer/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "synthesizer");
    // Structural ceiling is the whole safety story — every evergreen proposal
    // is downgraded to council regardless of confidence (ADR 0004).
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Structural,
        "synthesizer must be Structural (always council-routed)"
    );
    // 0.80 discard floor: below this the runner won't even convene a council.
    assert!(
        (cfg.confidence_threshold - 0.80).abs() < 1e-6,
        "expected 0.80 discard floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_synthesizer_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/synthesizer/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("synthesizer cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 synthesizer eval cases, got {}",
        cases.len()
    );
    // The decline case must exist — it's the safety case asserting Synthesizer
    // refuses to force a name onto an incoherent cluster. Dropping it would
    // silently lose coverage of the over-naming failure mode.
    assert!(
        cases.iter().any(|c| c.id.contains("decline")),
        "synthesizer cases must include the incoherent-cluster decline case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/synthesizer/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_synthesizer_prompt_loads() {
    let path = repo_root().join("agents/synthesizer/prompt.md");
    let structured = engram_agents::prompt_loader::load(&path).expect("prompt must load");
    assert!(
        !structured.static_head.is_empty(),
        "static head must not be empty"
    );
    assert!(
        !structured.dynamic_tail.is_empty(),
        "dynamic tail must not be empty"
    );
    // The static head MUST include the role declaration; otherwise the marker
    // landed before something important.
    assert!(
        structured.static_head.contains("Synthesizer"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable;
    // that's the whole point of the head/tail split.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted *propose* output (coherent cluster → one
/// evergreen) round-trips through the runner. Asserts the agent_runs row lands
/// with the correlation_id and the no-agent-commits invariant holds.
#[tokio::test]
async fn synthesizer_e2e_propose_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_synthesizer(vault.path());

    // Shape-matches `SynthesizerOutput` (mirrors the checked-in happy.json
    // fixture). The runner only parses `confidence` / `rationale` in the
    // current slice; the payload rides along. The round-trip below also asserts
    // the JSON parses back into the typed shape.
    let response = r###"{
        "confidence": 0.86,
        "rationale": "Five notes from this quarter reach for the same concept — the editor's choice of what to drop — without naming it. The proposed evergreen pulls forward what they all imply.",
        "decline": false,
        "cluster_coherence": {
            "coherent": true,
            "secondary_concept": null
        },
        "proposed_evergreen": {
            "title": "Editing as compression",
            "slug": "editing-as-compression",
            "body": "Editing is the editor's choice of what to drop. The choice is the work. See [[01H8X9]] for the rate-distortion analogy.",
            "source_note_ids": ["01H8X9", "01H8XA", "01H8XB", "01H8XC", "01H8XD"],
            "related_existing_evergreens": ["01H7AA", "01H7AB"]
        }
    }"###;

    // Sanity: the response parses into the typed output. If `SynthesizerOutput`
    // ever drifts from this canonical shape the assertion fires before the run.
    let parsed: SynthesizerOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.86).abs() < 1e-6);
    assert!(!parsed.decline, "coherent cluster must not decline");
    assert!(
        parsed.proposed_evergreen.is_some(),
        "a non-decline output must carry a proposed evergreen"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("synthesizer", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed");

    // The run completed cleanly (the Structural→council downgrade is a future
    // runner slice; today the relevant invariant is that the run was recorded
    // without error/panic).
    assert!(
        !matches!(report.outcome, RunOutcome::Errored | RunOutcome::Panicked),
        "run must complete cleanly; got {:?}",
        report.outcome
    );

    // Correlation ID invariant — non-empty ULID distinct from run_id.
    assert!(!report.correlation_id.is_empty(), "correlation_id required");
    assert_ne!(
        report.correlation_id, report.run_id,
        "correlation_id must be distinct from run_id"
    );

    // The `agent_runs` row must persist with the same run_id and correlation_id.
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
    assert_eq!(row_agent, "synthesizer");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    // No-agent-commits invariant (ADR 0003): the vault root has no `.git/`.
    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted *decline* output (incoherent cluster → no proposed evergreen)
/// round-trips through the runner. Covers the second output shape — a decline
/// carries no payload and reports low confidence.
#[tokio::test]
async fn synthesizer_e2e_decline_round_trips_without_payload() {
    let (sqlite, vault) = setup();
    install_synthesizer(vault.path());

    let response = r###"{
        "confidence": 0.0,
        "rationale": "The cluster contains two distinct concepts that share vocabulary; the embedding similarity is coincidental. Naming one would suppress the other, so declining lets the runner re-cluster around the secondary concept next sweep.",
        "decline": true,
        "cluster_coherence": {
            "coherent": false,
            "secondary_concept": "tape-archive compression formats"
        }
    }"###;

    let parsed: SynthesizerOutput = serde_json::from_str(response).expect("typed parse");
    assert!(parsed.decline, "incoherent cluster must decline");
    assert!(
        parsed.proposed_evergreen.is_none(),
        "a decline must not carry a proposed evergreen"
    );
    assert!(
        !parsed.cluster_coherence.coherent,
        "decline implies the cluster is not coherent"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("synthesizer", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed on a decline response");

    assert!(!report.correlation_id.is_empty());
    // The agent_runs row must record this run too.
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'synthesizer'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "decline run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
