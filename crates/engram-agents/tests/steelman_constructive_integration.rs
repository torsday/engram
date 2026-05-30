//! End-to-end integration test for the Steelman constructive-role agent (#51).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/steelman-constructive/{prompt.md, config.toml}` files (copied from
//! the repo root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Steelman behavior:
//!
//! 1. The config parses against `AgentConfig`, projecting to the `Editorial`
//!    invasiveness ceiling (reframings route through council; only additive
//!    annotations auto-land) and the documented 0.85 auto-land floor.
//! 2. The `prompt.md` loads through `prompt_loader` (catches missing or
//!    malformed cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` and include the
//!    restraint case (an already-sound note → no reframing).
//! 4. A `SteelmanConstructiveOutput`-shaped response round-trips: the runner
//!    parses `confidence`, the `agent_runs` row lands with the `run_id`
//!    returned on `RunReport`, and `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created (ADR 0003).
//!
//! Covers both output shapes: a weak draft (propose annotation + reframing)
//! and an already-sound note (a clean "no defensible reframing"). The
//! annotation-auto-land / reframing-council split is a follow-up runner slice
//! — the same scope boundary at which the sibling agents (#52, #61, #49)
//! landed.
//!
//! See `docs/design/01-agents-and-council.md` §Steelman (constructive role)
//! and `crates/engram-agents/src/agents/steelman_constructive.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::steelman_constructive::SteelmanConstructiveOutput;
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
                input_tokens_total: 2_500,
                output_tokens: 1_000,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 2.5,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 3.0,
                total_cents: 5.5,
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
/// (with `agents/steelman-constructive/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/steelman-constructive/{prompt.md, config.toml}` from the
/// checked-in repo into the test vault's `agents/` directory so the runner can
/// load them. A copy (not a symlink) keeps the test hermetic and covers the
/// disk-load path.
fn install_steelman(vault: &Path) {
    let src = repo_root().join("agents").join("steelman-constructive");
    let dst = vault.join("agents").join("steelman-constructive");
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

/// The checked-in `agents/steelman-constructive/config.toml` must parse against
/// the runner's `AgentConfig` and project to the Editorial ceiling. A
/// regression that raised the ceiling would let in-paragraph reframings
/// auto-land instead of routing through council — exactly what ADR 0007's
/// rationality gate forbids.
#[test]
fn checked_in_steelman_config_parses() {
    let toml =
        std::fs::read_to_string(repo_root().join("agents/steelman-constructive/config.toml"))
            .expect("config.toml must exist at agents/steelman-constructive/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "steelman-constructive");
    // Editorial ceiling: annotations (additive) auto-land, reframings
    // (editorial) route through council.
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Editorial,
        "steelman must cap at Editorial (reframings need council)"
    );
    // 0.85 auto-land floor for additive annotations.
    assert!(
        (cfg.confidence_threshold - 0.85).abs() < 1e-6,
        "expected 0.85 auto-land floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_steelman_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/steelman-constructive/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("steelman cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 steelman eval cases, got {}",
        cases.len()
    );
    // The restraint case (already-sound note → no reframing) must exist — it's
    // the one asserting Steelman won't manufacture change for its own sake, the
    // core of ADR 0007's rationality gate.
    assert!(
        cases.iter().any(|c| c.id.contains("no-reframing")),
        "steelman cases must include the already-sound no-reframing case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/steelman-constructive/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_steelman_prompt_loads() {
    let path = repo_root().join("agents/steelman-constructive/prompt.md");
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
        structured.static_head.contains("Steelman"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted strengthen output (annotation + reframing)
/// round-trips through the runner. Asserts the agent_runs row lands with the
/// correlation_id and the no-agent-commits invariant holds.
#[tokio::test]
async fn steelman_e2e_strengthen_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_steelman(vault.path());

    // Shape-matches `SteelmanConstructiveOutput` (mirrors the checked-in
    // happy.json fixture). The runner parses `confidence` / `rationale`; the
    // payload rides along. The round-trip below also asserts it parses back.
    let response = r###"{
        "confidence": 0.87,
        "rationale": "Two neighbor notes (01H8QZ, 01H8RC) establish the load-bearing premise the draft is reaching for; the proposed reframing trades a hedge for the specific claim those notes already support.",
        "proposed_annotations": [
            {
                "anchor_text": "I think this generalizes",
                "insertion_context": "...maybe. I think this generalizes to other lossy-reduction systems.",
                "supporting_note_ids": ["01H8QZ", "01H8RC"]
            }
        ],
        "proposed_reframings": [
            {
                "original_excerpt": "I think this generalizes to other lossy-reduction systems.",
                "proposed_text": "This generalizes to every lossy-reduction system where the reducer chooses what to drop.",
                "rationale": "01H8QZ already commits to the stronger claim; the hedge in the draft is the only weak point."
            }
        ]
    }"###;

    let parsed: SteelmanConstructiveOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.87).abs() < 1e-6);
    assert_eq!(parsed.proposed_annotations.len(), 1);
    assert_eq!(parsed.proposed_reframings.len(), 1);

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent(
            "steelman-constructive",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("run_agent must succeed");

    // The run completed cleanly (the annotation/reframing routing split is a
    // future runner slice; today the invariant is a recorded, error-free run).
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
    assert_eq!(row_agent, "steelman-constructive");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted "no defensible reframing" output (already-sound note → empty
/// payload, low confidence) round-trips through the runner. Covers the
/// restraint shape — Steelman declining to change a sound note.
#[tokio::test]
async fn steelman_e2e_no_reframing_round_trips_with_empty_payload() {
    let (sqlite, vault) = setup();
    install_steelman(vault.path());

    let response = r###"{
        "confidence": 0.12,
        "rationale": "The note is already structurally sound: the central claim is sharp, citations are present and load-bearing, and the neighbors don't supply evidence for a stronger reframing. A clean 'no defensible reframing' is the right output."
    }"###;

    let parsed: SteelmanConstructiveOutput = serde_json::from_str(response).expect("typed parse");
    assert!(
        parsed.confidence < 0.5,
        "restraint output is low-confidence"
    );
    assert!(
        parsed.proposed_annotations.is_empty() && parsed.proposed_reframings.is_empty(),
        "an already-sound note yields no annotations and no reframings"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent(
            "steelman-constructive",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("run_agent must succeed on a no-reframing response");

    assert!(!report.correlation_id.is_empty());
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'steelman-constructive'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "no-reframing run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
