//! End-to-end integration test for the Voice Keeper agent (#58).
//!
//! Wires the agent-agnostic `AgentRunner` against the on-disk
//! `agents/voice-keeper/{prompt.md, config.toml}` files (copied from the repo
//! root into a tempdir) and a scripted LLM provider. Asserts the
//! no-agent-commits invariant + happy-path Voice Keeper behavior:
//!
//! 1. The config parses against `AgentConfig`, projecting to the `Editorial`
//!    invasiveness ceiling and the documented 0.92 auto-land floor — the
//!    highest of the editorial agents because voice rewrites change the
//!    author's own prose.
//! 2. The `prompt.md` loads through `prompt_loader` (catches missing or
//!    malformed cache-boundary marker).
//! 3. The seed eval cases load through `Case::load_dir` and include the
//!    model-update mode case (so both modes are covered).
//! 4. A `VoiceKeeperOutput`-shaped response round-trips: the runner parses
//!    `confidence`, the `agent_runs` row lands with the `run_id` returned on
//!    `RunReport`, and `correlation_id` is propagated.
//! 5. **No-agent-commits invariant**: no `.git/` is created (ADR 0003).
//!
//! Covers both modes: `review` (per-passage verdicts on agent-drafted prose)
//! and `model-update` (evolve the voice model from the author corpus). The
//! verdict-routing and voice-model write are a follow-up runner slice — the
//! same scope boundary at which the sibling agents (#52, #61, #49, #51)
//! landed.
//!
//! See `docs/design/01-agents-and-council.md` §Voice Keeper and
//! `crates/engram-agents/src/agents/voice_keeper.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::agents::voice_keeper::{VoiceKeeperMode, VoiceKeeperOutput};
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
                input_tokens_total: 2_800,
                output_tokens: 1_100,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 2.8,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 3.2,
                total_cents: 6.0,
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
/// (with `agents/voice-keeper/`) is two parents up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root must be two parents up from manifest dir")
        .to_path_buf()
}

/// Copy `agents/voice-keeper/{prompt.md, config.toml}` from the checked-in repo
/// into the test vault's `agents/` directory so the runner can load them. A
/// copy (not a symlink) keeps the test hermetic and covers the disk-load path.
fn install_voice_keeper(vault: &Path) {
    let src = repo_root().join("agents").join("voice-keeper");
    let dst = vault.join("agents").join("voice-keeper");
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

/// The checked-in `agents/voice-keeper/config.toml` must parse against the
/// runner's `AgentConfig` and project to the Editorial ceiling with the highest
/// auto-land floor of the editorial agents — voice rewrites change the author's
/// own prose, so the bar to act without review is deliberately high.
#[test]
fn checked_in_voice_keeper_config_parses() {
    let toml = std::fs::read_to_string(repo_root().join("agents/voice-keeper/config.toml"))
        .expect("config.toml must exist at agents/voice-keeper/");
    let cfg = AgentConfig::from_toml(&toml).expect("config.toml must parse");
    assert_eq!(cfg.name, "voice-keeper");
    assert_eq!(
        cfg.max_invasiveness,
        Invasiveness::Editorial,
        "voice-keeper must cap at Editorial (rewrites route through council)"
    );
    // 0.92 floor — above the constructive Steelman (0.85) because a bad voice
    // rewrite erodes the very thing this agent protects.
    assert!(
        (cfg.confidence_threshold - 0.92).abs() < 1e-6,
        "expected 0.92 auto-land floor, got {}",
        cfg.confidence_threshold
    );
}

/// The checked-in seed eval cases must load through `Case::load_dir`. Catches
/// typos in keys, missing `id` fields, or YAML parse breakage before they show
/// up in a scorecard run.
#[test]
fn checked_in_voice_keeper_eval_cases_load() {
    let cases_dir = repo_root().join(".engram/evals/voice-keeper/cases");
    let cases = engram_eval::Case::load_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("voice-keeper cases must load from {cases_dir:?}: {e}"));
    // AC requires 5–10 cases. Pin the lower bound; the upper bound is soft.
    assert!(
        cases.len() >= 5,
        "expected ≥ 5 voice-keeper eval cases, got {}",
        cases.len()
    );
    // A model-update case must exist — Voice Keeper has two modes (review +
    // model-update) and dropping the model-update coverage would silently leave
    // half the agent untested.
    assert!(
        cases.iter().any(|c| c.id.contains("model-update")),
        "voice-keeper cases must cover model-update mode; got ids: {:?}",
        cases.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The checked-in `agents/voice-keeper/prompt.md` must load through
/// `prompt_loader::load`. Catches missing/duplicate cache-boundary marker
/// regressions.
#[test]
fn checked_in_voice_keeper_prompt_loads() {
    let path = repo_root().join("agents/voice-keeper/prompt.md");
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
        structured.static_head.contains("Voice Keeper"),
        "static head must declare the agent's role"
    );
    // The dynamic tail MUST contain at least one `{{...}}` template variable.
    assert!(
        structured.dynamic_tail.contains("{{"),
        "dynamic tail must have at least one template var"
    );
}

/// Full happy-path: a scripted `review` output (per-passage verdicts, one
/// propose_rewrite + one pass) round-trips through the runner. Asserts the
/// agent_runs row lands with the correlation_id and the no-agent-commits
/// invariant holds.
#[tokio::test]
async fn voice_keeper_e2e_review_round_trip_records_run_and_correlation_id() {
    let (sqlite, vault) = setup();
    install_voice_keeper(vault.path());

    // Shape-matches `VoiceKeeperOutput` (mirrors the checked-in happy.json
    // fixture). The runner parses `confidence` / `rationale`; the payload rides
    // along. The round-trip below also asserts it parses back into the type.
    let response = r###"{
        "confidence": 0.88,
        "rationale": "The draft opens with an abstract claim where the author always opens with a concrete example; the rest of the passage matches voice.",
        "mode": "review",
        "verdicts": [
            {
                "passage_excerpt": "Compression is the fundamental abstraction of writing.",
                "verdict": "propose_rewrite",
                "voice_signals": ["opens-abstract", "fundamental-overuse"],
                "proposed_rewrite": "When I cut the third draft down from 1200 words to 400, I noticed I was doing the same thing the note describes."
            },
            {
                "passage_excerpt": "I noticed I was doing the same thing the note describes.",
                "verdict": "pass",
                "voice_signals": ["concrete-example-voice"],
                "proposed_rewrite": null
            }
        ]
    }"###;

    let parsed: VoiceKeeperOutput = serde_json::from_str(response).expect("typed parse");
    assert!((parsed.confidence - 0.88).abs() < 1e-6);
    assert_eq!(parsed.mode, VoiceKeeperMode::Review);
    assert_eq!(parsed.verdicts.len(), 2);
    assert!(
        parsed.verdicts.iter().any(|v| v.proposed_rewrite.is_some()),
        "review of a drifting passage must propose at least one rewrite"
    );

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("voice-keeper", TriggerContext::OnDemand { note_id: None })
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
    assert_eq!(row_agent, "voice-keeper");
    assert!(!row_outcome.is_empty(), "outcome must be recorded");

    let git_dir = vault.path().join(".git");
    assert!(
        !git_dir.exists(),
        ".git must not be created — agents must never touch git history"
    );
}

/// A scripted `model-update` output (propose additions + a retirement to the
/// voice model) round-trips through the runner. Covers the second mode — a
/// model update carries no per-passage verdicts.
#[tokio::test]
async fn voice_keeper_e2e_model_update_round_trips() {
    let (sqlite, vault) = setup();
    install_voice_keeper(vault.path());

    let response = r###"{
        "confidence": 0.81,
        "rationale": "The recent author-written corpus added two distinctive patterns and dropped one; the dropped pattern is the sharper signal and is flagged for human confirmation.",
        "mode": "model-update",
        "model_update": {
            "additions": ["prefers-em-dash-over-colon", "double-quoted-keywords"],
            "retirements": ["explicit-numbered-lists-in-prose"],
            "rationale": "The two additions are stable across the last 30 author-written notes; the retirement is sharper and human approval is recommended."
        }
    }"###;

    let parsed: VoiceKeeperOutput = serde_json::from_str(response).expect("typed parse");
    assert_eq!(parsed.mode, VoiceKeeperMode::ModelUpdate);
    assert!(
        parsed.verdicts.is_empty(),
        "a model-update carries no per-passage verdicts"
    );
    let update = parsed
        .model_update
        .expect("model-update mode must carry a model_update payload");
    assert_eq!(update.additions.len(), 2);
    assert_eq!(update.retirements.len(), 1);

    let provider = Arc::new(Scripted {
        responses: StdMutex::new(vec![response.to_string()]),
    });
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("voice-keeper", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed on a model-update response");

    assert!(!report.correlation_id.is_empty());
    let count: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE id = ?1 AND agent_name = 'voice-keeper'",
            [&report.run_id],
            |r| r.get(0),
        )
        .expect("query agent_runs")
    };
    assert_eq!(count, 1, "model-update run must be recorded exactly once");

    let git_dir = vault.path().join(".git");
    assert!(!git_dir.exists(), ".git must not be created");
}
