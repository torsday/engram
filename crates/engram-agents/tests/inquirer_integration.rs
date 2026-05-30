//! End-to-end integration test for the Inquirer agent (#52).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/inquirer/{prompt.md, config.toml}` files (copied from the repo
//! root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Inquirer behavior:
//!
//! 1. The Inquirer's `config.toml` parses against `AgentConfig` without
//!    error (catches regressions to either schema), projecting to the
//!    inbox-only invasiveness ceiling (`Additive`) and the documented
//!    auto-land floor.
//! 2. The `prompt.md` loads through `prompt_loader` (catches missing or
//!    malformed cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` (catches YAML
//!    breakage before a scorecard run) and include the calibration case.
//! 4. An `InquirerOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id` returned
//!    on `RunReport`, and `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created, no commit or
//!    stage operation is performed. Inquirer is an inbox-only agent, so
//!    ADR 0003's contract must hold on its path too.
//!
//! The four-mode dispatch and per-mode output-path writing live in a
//! follow-up runner slice; this test exercises the agent-load + invoke +
//! record path that is wired today, against two of the four modes
//! (daily-reactive and blindspot) to cover both the single-question and
//! empty-`motivating_note_ids` shapes.
//!
//! See `docs/design/01-agents-and-council.md` §Inquirer for the agent's
//! spec and `crates/engram-agents/src/agents/inquirer.rs` for the Rust
//! types.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::inquirer::{InquirerMode, InquirerOutput};
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
                input_tokens_total: 2_000,
                output_tokens: 900,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 2.0,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 2.5,
                total_cents: 4.5,
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
/// (with `agents/inquirer/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/inquirer/{prompt.md, config.toml}` from the checked-in repo
/// into the test vault's `agents/` directory so the runner can load them.
/// Done as a copy rather than a symlink so the test is hermetic and the
/// integration covers the disk-load path.
fn install_inquirer(vault: &Path) {
    let src = repo_root().join("agents").join("inquirer");
    let dst = vault.join("agents").join("inquirer");
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

/// The checked-in `agents/inquirer/config.toml` must parse against the runner's
/// `AgentConfig` schema. Inquirer uses the nested (ADR 0017) config shape; a
/// drift in either deserializer would manifest as a hard-to-debug runtime
/// failure in production, so surface it here.
#[test]
fn checked_in_inquirer_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/inquirer/config.toml"))
        .expect("config.toml must exist at agents/inquirer/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "inquirer");
    // Inbox-only agent: its ceiling must project to Additive. A regression that
    // let Inquirer claim a higher invasiveness would weaken the safety story.
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Additive,
        "inquirer must be inbox-only (Additive ceiling)"
    );
    // Auto-land floor documented in the config (0.75) — below the editorial
    // agents because a bad inbox question is cheap.
    assert!(
        (cfg.confidence_threshold - 0.75).abs() < 1e-6,
        "expected 0.75 auto-land floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_inquirer_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/inquirer/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("inquirer cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 inquirer eval cases, got {}",
        cases.len()
    );
    // The calibration case (sparse vault → honest low confidence) must exist —
    // it's the one asserting a confidence *ceiling*, and dropping it would
    // silently lose coverage of the over-confidence failure mode Watcher cares
    // about most.
    assert!(
        cases.iter().any(|c| c.id.contains("low-confidence")),
        "inquirer cases must include the sparse-vault calibration case; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/inquirer/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_inquirer_prompt_loads() {
    let path = repo_root().join("agents/inquirer/prompt.md");
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
        structured.static_head.contains("Inquirer"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable;
    // that's the whole point of the head/tail split.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted `daily-reactive` Inquirer output (exactly one
/// question) round-trips through the runner. Asserts the agent_runs row lands
/// with the correlation_id and the no-agent-commits invariant holds.
#[tokio::test]
async fn inquirer_e2e_daily_reactive_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_inquirer(vault.path());

    // Shape-matches `InquirerOutput`. The runner only parses `confidence` /
    // `rationale` in the current slice; everything else rides along. The
    // round-trip below also asserts the JSON parses back into the typed shape.
    let response = r###"{
        "confidence": 0.84,
        "rationale": "Today's edit to the attention note tightens a claim that sits in tension with last month's legibility thread; one sharp question surfaces that tension without resolving it.",
        "mode": "daily-reactive",
        "questions": [
            {
                "question": "If attention is the scarce resource, does that undercut the legibility argument in [[01JRZK4N8Q]] that structure is free?",
                "motivating_note_ids": ["01JRZK4N8Q", "01JRZK7T2V"],
                "why_now": "Today's edit sharpened the attention claim into direct tension with an earlier premise."
            }
        ],
        "output_path": "inbox/2026-05-30-attention-vs-legibility.md"
    }"###;

    // Sanity: the response parses into the typed output. If `InquirerOutput`
    // ever drifts from this canonical shape the assertion fires before the run.
    let parsed: InquirerOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.84).abs() < 1e-6);
    assert_eq!(parsed.mode, InquirerMode::DailyReactive);
    assert_eq!(
        parsed.questions.len(),
        1,
        "daily-reactive emits exactly one"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("inquirer", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed");

    // Inbox-only output with confidence ≥ threshold and no proposed changes
    // produces a clean decision in the current runner slice.
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
    assert_eq!(row_agent, "inquirer");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    // No-agent-commits invariant (ADR 0003): the vault root has no `.git/`
    // because we never initialized one; a regression that tried to `git add`
    // would create or write under `.git/`. Assert it does not exist.
    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted `blindspot` output (negative-space questions with empty
/// `motivating_note_ids`) round-trips through the runner. Covers the second
/// question shape — observations about an *absence* have no motivating note.
#[tokio::test]
async fn inquirer_e2e_blindspot_round_trips_with_empty_motivating_ids() {
    let (sqlite, vault) = setup();
    install_inquirer(vault.path());

    let response = r###"{
        "confidence": 0.71,
        "rationale": "Across the quarter McLuhan is cited five times but the tetrad framework is conspicuously absent, and systems-thinking is gestured at without a single dedicated note — both are real negative spaces.",
        "mode": "blindspot",
        "questions": [
            {
                "question": "What would McLuhan's tetrad reveal about your own note-taking system?",
                "motivating_note_ids": [],
                "why_now": "McLuhan is cited repeatedly but his central analytical framework is never applied."
            },
            {
                "question": "Where would a dedicated systems-thinking note change how the existing clusters connect?",
                "motivating_note_ids": [],
                "why_now": "Systems framing is gestured at across notes but never developed in one place."
            }
        ],
        "output_path": "reflections/blindspots-2026-Q2.md"
    }"###;

    let parsed: InquirerOutput = serde_json::from_str(response).expect("typed parse");
    assert_eq!(parsed.mode, InquirerMode::Blindspot);
    assert!(
        parsed
            .questions
            .iter()
            .all(|q| q.motivating_note_ids.is_empty()),
        "blindspot observations about absences carry no motivating note"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("inquirer", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed on a blindspot response");

    assert!(!report.correlation_id.is_empty());
    // The agent_runs row must record this run too.
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'inquirer'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "blindspot run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
