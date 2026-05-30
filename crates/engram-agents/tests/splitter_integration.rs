//! End-to-end integration test for the Splitter agent (#63).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/splitter/{prompt.md, config.toml}` files (copied from the repo root
//! into a tempdir) and a scripted LLM provider. Asserts the no-agent-commits
//! invariant + happy-path Splitter behavior:
//!
//! 1. The config parses against `AgentConfig`, projecting to the `Structural`
//!    invasiveness ceiling (every split is council-routed) and the 0.82 floor.
//! 2. The `prompt.md` loads through `prompt_loader` (cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` and include the
//!    single-sustained-argument decline case.
//! 4. A `SplitterOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id`, and
//!    `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created (ADR 0003).
//!
//! Covers both output shapes: a multi-concept split (resulting notes +
//! disambiguation residual) and a decline (single sustained argument → no
//! payload). The council routing of the Structural split is a follow-up runner
//! slice — the same scope boundary at which the sibling agents (#52, #61, #49,
//! #51, #58, #50, #56, #64) landed.
//!
//! See `docs/design/01-agents-and-council.md` §Splitter and
//! `crates/engram-agents/src/agents/splitter.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::splitter::SplitterOutput;
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
                output_tokens: 1_300,
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
/// (with `agents/splitter/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/splitter/{prompt.md, config.toml}` from the checked-in repo
/// into the test vault's `agents/` directory so the runner can load them.
fn install_splitter(vault: &Path) {
    let src = repo_root().join("agents").join("splitter");
    let dst = vault.join("agents").join("splitter");
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

/// The checked-in `agents/splitter/config.toml` must parse against the runner's
/// `AgentConfig` and project to the Structural ceiling — splitting one note
/// into several is a Structural change that routes through council, never
/// auto-lands.
#[test]
fn checked_in_splitter_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/splitter/config.toml"))
        .expect("config.toml must exist at agents/splitter/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "splitter");
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Structural,
        "splitter must be Structural (splits route through council)"
    );
    assert!(
        (cfg.confidence_threshold - 0.82).abs() < 1e-6,
        "expected 0.82 floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_splitter_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/splitter/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("splitter cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 splitter eval cases, got {}",
        cases.len()
    );
    // The single-sustained-argument decline case must exist — it's the safety
    // case asserting Splitter won't fragment a coherent argument.
    assert!(
        cases.iter().any(|c| c.id.contains("decline")),
        "splitter cases must include the single-argument decline case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/splitter/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_splitter_prompt_loads() {
    let path = repo_root().join("agents/splitter/prompt.md");
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
        structured.static_head.contains("Splitter"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted *split* output (two resulting notes +
/// disambiguation residual) round-trips through the runner. Asserts the
/// agent_runs row lands with the correlation_id and no-agent-commits holds.
#[tokio::test]
async fn splitter_e2e_split_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_splitter(vault.path());

    // Shape-matches `SplitterOutput` (mirrors the checked-in happy.json
    // fixture). The runner parses `confidence` / `rationale`; the payload rides
    // along. The round-trip below also asserts it parses back into the type.
    let response = r###"{
        "confidence": 0.83,
        "rationale": "The note opens with three paragraphs on rate-distortion theory and then pivots to editing as compression — two distinct concepts that share vocabulary.",
        "decline": false,
        "coherence_signals": ["two-heading-clusters", "mid-note-topic-shift"],
        "proposed_split": {
            "resulting_notes": [
                {
                    "title": "Rate-distortion theory",
                    "slug": "rate-distortion-theory",
                    "body": "Rate-distortion theory bounds the trade-off between fidelity and rate in a lossy channel.",
                    "moved_section_ids": ["s1", "s2", "s3"],
                    "incoming_link_assignment": [
                        { "source_note_id": "01H8AA", "anchor_text": "rate-distortion" }
                    ]
                },
                {
                    "title": "Editing as compression",
                    "slug": "editing-as-compression",
                    "body": "Editing is the editor's choice of what to drop.",
                    "moved_section_ids": ["s4", "s5"],
                    "incoming_link_assignment": [
                        { "source_note_id": "01H8AB", "anchor_text": "editing-as-compression" }
                    ]
                }
            ],
            "residual": {
                "kind": "disambiguation",
                "body": "This note was split into [[Rate-distortion theory]] and [[Editing as compression]]."
            },
            "unassigned_incoming_links": []
        }
    }"###;

    let parsed: SplitterOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.83).abs() < 1e-6);
    assert!(!parsed.decline, "a multi-concept note must not decline");
    let split = parsed
        .proposed_split
        .expect("a non-decline output must carry a proposed split");
    assert_eq!(
        split.resulting_notes.len(),
        2,
        "the two concepts split into two atomic notes"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("splitter", TriggerContext::OnDemand { note_id: None })
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
    assert_eq!(row_agent, "splitter");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted *decline* output (single sustained argument → no split)
/// round-trips through the runner. Covers the safety shape — a decline carries
/// only coherence signals, no proposed split.
#[tokio::test]
async fn splitter_e2e_decline_round_trips_without_payload() {
    let (sqlite, vault) = setup();
    install_splitter(vault.path());

    let response = r###"{
        "confidence": 0.0,
        "rationale": "The note is one sustained argument from premise to conclusion; the three sections build on each other rather than treating distinct concepts. The continuous citation thread and single central claim argue against splitting.",
        "decline": true,
        "coherence_signals": ["single-sustained-argument", "continuous-citation-thread"]
    }"###;

    let parsed: SplitterOutput = serde_json::from_str(response).expect("typed parse");
    assert!(parsed.decline, "a single sustained argument must decline");
    assert!(
        parsed.proposed_split.is_none(),
        "a decline must not carry a proposed split"
    );
    assert!(
        !parsed.coherence_signals.is_empty(),
        "a decline still reports the coherence signals that drove it"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("splitter", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed on a decline response");

    assert!(!report.correlation_id.is_empty());
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'splitter'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "decline run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
