//! End-to-end integration test for the Pair-Thinking agent (#56).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/pair-thinking/{prompt.md, config.toml}` files (copied from the repo
//! root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Pair-Thinking behavior:
//!
//! 1. The config parses against `AgentConfig`, projecting to the `Mechanical`
//!    invasiveness ceiling and the documented 0.82 floor.
//! 2. The `prompt.md` loads through `prompt_loader` (cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` and include the
//!    bounded-session end case.
//! 4. A `PairThinkingTurn`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id`, and
//!    `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created (ADR 0003).
//!
//! Covers both turn shapes: a mid-session question turn (`connect` mode,
//! should_end=false) and a session-end turn (`end` mode, should_end=true,
//! empty question). The multi-round session orchestration + token streaming
//! (#23 / #36) are a follow-up runner slice — the same scope boundary at which
//! the sibling agents (#52, #61, #49, #51, #58, #50) landed.
//!
//! See `docs/design/01-agents-and-council.md` §Pair-Thinking and
//! `crates/engram-agents/src/agents/pair_thinking.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::pair_thinking::{PairThinkingMode, PairThinkingTurn};
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
                input_tokens_total: 1_800,
                output_tokens: 400,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 1.8,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 1.0,
                total_cents: 2.8,
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
/// (with `agents/pair-thinking/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/pair-thinking/{prompt.md, config.toml}` from the checked-in
/// repo into the test vault's `agents/` directory so the runner can load them.
fn install_pair_thinking(vault: &Path) {
    let src = repo_root().join("agents").join("pair-thinking");
    let dst = vault.join("agents").join("pair-thinking");
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

/// The checked-in `agents/pair-thinking/config.toml` must parse against the
/// runner's `AgentConfig` and project to the Mechanical ceiling — Pair-Thinking
/// only inserts inline questions while the author is writing, the lightest
/// invasiveness class.
#[test]
fn checked_in_pair_thinking_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/pair-thinking/config.toml"))
        .expect("config.toml must exist at agents/pair-thinking/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "pair-thinking");
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Mechanical,
        "pair-thinking must cap at Mechanical (inline questions only)"
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
fn checked_in_pair_thinking_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/pair-thinking/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("pair-thinking cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 pair-thinking eval cases, got {}",
        cases.len()
    );
    // The bounded-session end case must exist — knowing when to stop is the
    // discipline that keeps a conversational agent from outstaying its welcome.
    assert!(
        cases.iter().any(|c| c.id.contains("session-end")),
        "pair-thinking cases must include the bounded-session end case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/pair-thinking/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_pair_thinking_prompt_loads() {
    let path = repo_root().join("agents/pair-thinking/prompt.md");
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
        structured.static_head.contains("Pair-Thinking"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted mid-session question turn (`connect` mode)
/// round-trips through the runner. Asserts the agent_runs row lands with the
/// correlation_id and no-agent-commits holds.
#[tokio::test]
async fn pair_thinking_e2e_question_turn_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_pair_thinking(vault.path());

    // Shape-matches `PairThinkingTurn` (mirrors the checked-in happy.json
    // fixture). The runner parses `confidence` / `rationale`; the payload rides
    // along. The round-trip below also asserts it parses back into the type.
    let response = r###"{
        "confidence": 0.83,
        "rationale": "The paragraph reaches for a connection to information theory without naming it; a connect-mode question can surface 01H8X9 (rate-distortion) which the author has already engaged.",
        "round": 2,
        "mode": "connect",
        "question": "Does the lossy-compression framing here connect to your rate-distortion note (01H8X9), or are you reaching for a different theoretical lineage?",
        "should_end": false,
        "referenced_note_ids": ["01H8X9"]
    }"###;

    let parsed: PairThinkingTurn = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.83).abs() < 1e-6);
    assert_eq!(parsed.mode, PairThinkingMode::Connect);
    assert!(
        !parsed.should_end,
        "a mid-session turn must not end the session"
    );
    assert!(
        !parsed.question.is_empty(),
        "a question turn carries a question"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("pair-thinking", TriggerContext::OnDemand { note_id: None })
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
    assert_eq!(row_agent, "pair-thinking");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted session-end turn (`end` mode, should_end=true, empty question)
/// round-trips through the runner. Covers the bounded-session restraint shape —
/// the agent deciding to stop.
#[tokio::test]
async fn pair_thinking_e2e_session_end_turn_round_trips() {
    let (sqlite, vault) = setup();
    install_pair_thinking(vault.path());

    let response = r###"{
        "confidence": 0.92,
        "rationale": "The last two rounds produced sharp clarifications and the draft now reads coherently. Further questions would push past the productive frontier; ending early respects the bounded-session budget.",
        "round": 3,
        "mode": "end",
        "question": "",
        "should_end": true
    }"###;

    let parsed: PairThinkingTurn = serde_json::from_str(response).expect("typed parse");
    assert_eq!(parsed.mode, PairThinkingMode::End);
    assert!(parsed.should_end, "an end turn must end the session");
    assert!(
        parsed.question.is_empty(),
        "an end turn carries no question"
    );
    assert!(
        parsed.referenced_note_ids.is_empty(),
        "the default applies when an end turn omits referenced_note_ids"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("pair-thinking", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed on a session-end turn");

    assert!(!report.correlation_id.is_empty());
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'pair-thinking'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "session-end turn must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
