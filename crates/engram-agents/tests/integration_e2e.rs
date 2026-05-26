//! End-to-end integration test for `AgentRunner` (#27 AC last
//! bullet).
//!
//! Wires the runner against a fixture vault on disk + a scripted LLM
//! provider, and asserts the full per-invocation contract — every
//! signal that downstream consumers (the future review queue, the
//! action-log reconciler, the tracing pipeline) depend on:
//!
//! 1. `agent_runs` row lands with the same `run_id` the runner
//!    returned on `RunReport`
//! 2. `correlation_id` is populated on the `RunReport` (a non-empty
//!    ULID distinct from `run_id`) so downstream sub-agent calls
//!    and tracing spans can share it. A future schema slice will
//!    persist it onto `agent_runs` as well — when that happens this
//!    test will tighten to assert the DB column.
//! 3. AutoLand path: file lands on disk under the vault root AND an
//!    `agent_actions` row joins back to it via `RunReport.action_id`
//! 4. No-AutoLand path: proposal JSON lands at
//!    `.engram/proposals/<id>.json` AND a `proposals` row joins back
//!    to it via `RunReport.proposal_id`, with `status = 'pending'`
//!
//! This is a *black-box* test (`tests/` directory, not `src/`) so it
//! only touches the public API. If a future refactor breaks any of
//! the four assertions, this test trips before any consumer does.

use std::path::Path;
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::locks::{LockConfig, LockManager};
use engram_agents::runner::{AgentRunner, RunOutcome, TriggerContext};
use engram_index::sqlite::Migrator;
use engram_llm::{
    CompleteOptions, Completion, Cost, EmbeddingModel, LlmProvider, Model, ModelProvider,
    PromptStructured, StreamedCompletion, Usage,
};
use rusqlite::Connection;
use tempfile::tempdir;

/// Scripted provider that returns a queued response per call. Mirrors
/// the one in the runner's unit tests but lives in this crate's
/// integration-test surface (production-side `engram_llm` doesn't
/// expose its mocks under the right cfg gate for an external `tests/`
/// file). Kept private; one user, no API surface to maintain.
struct Scripted {
    responses: StdMutex<Vec<&'static str>>,
}

impl Scripted {
    fn new(responses: Vec<&'static str>) -> Self {
        Self {
            responses: StdMutex::new(responses),
        }
    }
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
            q.remove(0).to_string()
        };
        Ok(Completion {
            text,
            usage: Usage {
                input_tokens_total: 100,
                output_tokens: 25,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 1.0,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 2.0,
                total_cents: 3.0,
            },
            model_used: format!("mock/{}", model.name),
            latency_ms: 1,
        })
    }

    async fn complete_streamed(
        &self,
        _prompt: &PromptStructured,
        _model: &Model,
        _options: &CompleteOptions,
    ) -> engram_llm::Result<StreamedCompletion> {
        unreachable!("integration test never streams");
    }

    async fn embed(&self, _text: &str, _model: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
        unreachable!("integration test never embeds");
    }
}

fn write_agent(root: &Path, name: &str, threshold: &str) {
    let dir = root.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            r#"name = "{name}"
trigger = "on_demand"
confidence_threshold = {threshold}
"#
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("prompt.md"),
        "static head\n<!-- /cache -->\ndynamic tail trigger={{trigger}}\n",
    )
    .unwrap();
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

/// Full happy-path: AutoLand. Asserts the 5 things the AC requires
/// (run row, correlation id propagation, file on disk, action row,
/// no proposal).
#[tokio::test]
async fn autoland_e2e_persists_run_action_and_file() {
    let (sqlite, vault) = setup();
    write_agent(vault.path(), "linker", "0.7");
    let provider = Arc::new(Scripted::new(vec![
        r##"{
        "confidence": 0.95,
        "kind": "link-suggestion",
        "rationale": "alpha references beta",
        "proposed_changes": [
            {"path": "notes/alpha.md", "new_content": "# Alpha\n\nbody."}
        ]
    }"##,
    ]));
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("linker", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed");

    // 1. Outcome is AutoLand.
    assert_eq!(report.outcome, RunOutcome::AutoLand);

    // 2. agent_runs row exists for this run_id. correlation_id is
    //    surfaced on the RunReport (and into tracing spans via
    //    `info_span!` in the runner) — the DB row keys on `run_id`,
    //    so we assert the run row is present + the correlation_id
    //    is a distinct non-empty ULID propagated into the report.
    assert!(
        !report.correlation_id.is_empty(),
        "correlation_id must be populated"
    );
    assert_ne!(
        report.correlation_id, report.run_id,
        "correlation_id and run_id are distinct identifiers"
    );
    let conn = sqlite.lock().unwrap();
    let (agent_name, outcome): (String, String) = conn
        .query_row(
            "SELECT agent_name, outcome FROM agent_runs WHERE id = ?1",
            rusqlite::params![report.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("agent_runs row must exist");
    assert_eq!(agent_name, "linker");
    assert_eq!(outcome, "auto_land");
    drop(conn);

    // 3. File landed on disk under vault root.
    let landed = vault.path().join("notes/alpha.md");
    assert!(landed.exists(), "notes/alpha.md must land on disk");
    assert_eq!(
        std::fs::read_to_string(&landed).unwrap(),
        "# Alpha\n\nbody."
    );

    // 4. agent_actions row joined back via RunReport.action_id.
    let action_id = report
        .action_id
        .as_ref()
        .expect("AutoLand with landed file must record an action row");
    let conn = sqlite.lock().unwrap();
    let (action_agent, kind, conf): (String, String, f64) = conn
        .query_row(
            "SELECT agent_name, kind, confidence FROM agent_actions WHERE id = ?1",
            rusqlite::params![action_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("agent_actions row must exist for the AutoLand");
    assert_eq!(action_agent, "linker");
    assert_eq!(kind, "link-suggestion");
    assert!((conf - 0.95).abs() < 1e-6);

    // 5. No proposal is filed for AutoLand.
    assert!(report.proposal_id.is_none());
    let proposal_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM proposals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(proposal_count, 0);
}

/// Full No-AutoLand path: NoAction outcome + proposal filer. Asserts
/// the 4 things the AC requires for the gated-down case (run row,
/// correlation id propagation, proposal JSON on disk, proposals row).
#[tokio::test]
async fn noaction_e2e_files_proposal_json_and_row() {
    let (sqlite, vault) = setup();
    // Threshold above the agent's confidence so the run gates down.
    write_agent(vault.path(), "linker", "0.99");
    let provider = Arc::new(Scripted::new(vec![
        r##"{
        "confidence": 0.5,
        "kind": "link-suggestion",
        "rationale": "weak hit; needs review",
        "proposed_changes": [
            {"path": "notes/maybe.md", "new_content": "# Maybe\n"}
        ]
    }"##,
    ]));
    let runner = make_runner(&sqlite, provider, vault.path());

    let report = runner
        .run_agent("linker", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("run_agent must succeed");

    // 1. Outcome is NoAction.
    assert_eq!(report.outcome, RunOutcome::NoAction);

    // 2. agent_runs row exists + correlation_id surfaced on report.
    assert!(!report.correlation_id.is_empty());
    let conn = sqlite.lock().unwrap();
    let outcome: String = conn
        .query_row(
            "SELECT outcome FROM agent_runs WHERE id = ?1",
            rusqlite::params![report.run_id],
            |r| r.get(0),
        )
        .expect("agent_runs row must exist for the NoAction run");
    assert_eq!(outcome, "no_action");
    drop(conn);

    // 3. No file landed on disk.
    assert!(!vault.path().join("notes/maybe.md").exists());

    // 4. proposals row + JSON artifact exist, joined by proposal_id.
    let proposal_id = report
        .proposal_id
        .as_ref()
        .expect("NoAction with proposed_changes must file a proposal");
    let conn = sqlite.lock().unwrap();
    let (agent, status, diff_path, rationale): (String, String, String, String) = conn
        .query_row(
            "SELECT proposing_agent, status, proposed_diff_path, rationale \
             FROM proposals WHERE id = ?1",
            rusqlite::params![proposal_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("proposals row must exist");
    assert_eq!(agent, "linker");
    assert_eq!(status, "pending");
    assert_eq!(rationale, "weak hit; needs review");

    let json_path = Path::new(&diff_path);
    assert!(
        json_path.exists(),
        "proposal JSON at {} must exist on disk",
        json_path.display()
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(json_path).unwrap()).unwrap();
    assert_eq!(parsed["id"].as_str().unwrap(), proposal_id);
    assert_eq!(parsed["proposing_agent"].as_str().unwrap(), "linker");
    assert!(
        (parsed["confidence"].as_f64().unwrap() - 0.5).abs() < 1e-6,
        "confidence must round-trip into the JSON"
    );

    // No agent_actions row — no on-disk effect to audit.
    let action_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM agent_actions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(action_count, 0);
}

/// Two runs of the same agent in sequence produce two distinct
/// correlation_ids and two distinct run_ids — the runner doesn't
/// memoize across calls.
#[tokio::test]
async fn back_to_back_runs_get_distinct_correlation_ids() {
    let (sqlite, vault) = setup();
    write_agent(vault.path(), "linker", "0.99");
    let provider = Arc::new(Scripted::new(vec![
        r#"{"confidence": 0.1}"#,
        r#"{"confidence": 0.1}"#,
    ]));
    let runner = make_runner(&sqlite, provider, vault.path());

    let r1 = runner
        .run_agent("linker", TriggerContext::OnDemand { note_id: None })
        .await
        .unwrap();
    let r2 = runner
        .run_agent("linker", TriggerContext::OnDemand { note_id: None })
        .await
        .unwrap();

    assert_ne!(r1.run_id, r2.run_id);
    assert_ne!(r1.correlation_id, r2.correlation_id);

    let conn = sqlite.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_runs WHERE agent_name = ?1",
            rusqlite::params!["linker"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}
