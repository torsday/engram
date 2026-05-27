//! End-to-end integration test for the inter-agent sub-agent
//! invocation contract (#31). Simulates the AC's "Curator-style
//! parent invokes 3 sub-agents in sequence" scenario by chaining
//! `run_sub_agent` calls (parent → sub1 → sub2 → sub3), then
//! asserting the persistence layer correctly stitches them:
//!
//! 1. All four runs share one `correlation_id` (the parent's).
//! 2. `agent_runs.parent_run_id` chains correctly through the
//!    generations (parent has NULL; sub1 → parent.id; sub2 →
//!    sub1.id; sub3 → sub2.id).
//! 3. `agent_actions.parent_run_id` on each sub's AutoLand audit
//!    row points at the immediate parent's run_id.
//! 4. A sub-agent invocation at depth `MAX_SUB_AGENT_DEPTH + 1` is
//!    rejected with `RecursionLimitExceeded` before any DB writes.
//!
//! The test doesn't currently exercise the "agent runs and that
//! agent's invocation triggers a sub-agent call" path — that
//! requires a runner-aware provider, which is a future slice. The
//! contract that matters for downstream consumers (correlation +
//! parent chaining) is the same either way.

use std::path::Path;
use std::sync::{Arc, Mutex, Mutex as StdMutex};

use async_trait::async_trait;
use engram_agents::locks::{LockConfig, LockManager};
use engram_agents::runner::{
    AgentRunner, RunOutcome, RunnerError, TriggerContext, DEFAULT_SUB_AGENT_TIMEOUT,
    MAX_SUB_AGENT_DEPTH,
};
use engram_index::sqlite::Migrator;
use engram_llm::{
    CompleteOptions, Completion, Cost, EmbeddingModel, LlmProvider, Model, ModelProvider,
    PromptStructured, StreamedCompletion, Usage,
};
use rusqlite::Connection;
use tempfile::tempdir;

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
                input_tokens_total: 10,
                output_tokens: 5,
                ..Default::default()
            },
            cost: Cost {
                input_cents: 0.1,
                cache_create_cents: 0.0,
                cache_read_cents: 0.0,
                output_cents: 0.2,
                total_cents: 0.3,
            },
            model_used: format!("mock/{}", model.name),
            latency_ms: 0,
        })
    }
    async fn complete_streamed(
        &self,
        _prompt: &PromptStructured,
        _model: &Model,
        _options: &CompleteOptions,
    ) -> engram_llm::Result<StreamedCompletion> {
        unreachable!()
    }
    async fn embed(&self, _text: &str, _model: &EmbeddingModel) -> engram_llm::Result<Vec<f32>> {
        unreachable!()
    }
}

fn write_agent(root: &Path, name: &str) {
    let dir = root.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!("name = \"{name}\"\ntrigger = \"on_demand\"\nconfidence_threshold = 0.7\n"),
    )
    .unwrap();
    std::fs::write(dir.join("prompt.md"), "head\n<!-- /cache -->\ntail\n").unwrap();
}

fn setup() -> (Arc<Mutex<Connection>>, tempfile::TempDir) {
    let tmp = tempdir().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    Migrator::new(&conn).apply_all().unwrap();
    (Arc::new(Mutex::new(conn)), tmp)
}

fn make_runner(
    sqlite: &Arc<Mutex<Connection>>,
    responses: Vec<&'static str>,
    vault: &Path,
) -> AgentRunner {
    AgentRunner::new(
        Arc::clone(sqlite),
        Arc::new(Scripted::new(responses)),
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

const AUTOLAND_RESPONSE: &str = r##"{
    "confidence": 0.95,
    "kind": "chain-step",
    "rationale": "chain step",
    "proposed_changes": [
        {"path": "notes/step.md", "new_content": "step"}
    ]
}"##;

/// Curator-style parent → sub1 → sub2 → sub3 chain.
///
/// Each level produces an AutoLand run and an agent_actions row.
/// Correlation ID flows from parent through every sub; parent_run_id
/// chains the agent_runs table so a SQL join reconstructs the call
/// tree.
#[tokio::test]
async fn three_level_sub_agent_chain_persists_correlation_and_parent_chain() {
    let (sqlite, vault) = setup();
    for name in ["curator", "sub1", "sub2", "sub3"] {
        write_agent(vault.path(), name);
    }
    // 4 scripted responses: one per run (parent + 3 subs). The
    // path varies so AutoLand doesn't collide on the same file.
    let mut responses = Vec::new();
    for name in ["curator", "sub1", "sub2", "sub3"] {
        responses.push(Box::leak(
            format!(
                r##"{{"confidence":0.95,"kind":"chain","rationale":"r","proposed_changes":[{{"path":"notes/{name}.md","new_content":"x"}}]}}"##
            )
            .into_boxed_str(),
        ) as &'static str);
    }
    let runner = make_runner(&sqlite, responses, vault.path());

    // 1. Parent (top-level) run.
    let parent = runner
        .run_agent("curator", TriggerContext::OnDemand { note_id: None })
        .await
        .expect("parent runs cleanly");
    assert_eq!(parent.outcome, RunOutcome::AutoLand);
    assert_eq!(parent.sub_agent_depth, 0);

    // 2. sub1 invoked by parent (depth 1).
    let sub1 = runner
        .run_sub_agent(
            &parent.run_id,
            &parent.correlation_id,
            1,
            DEFAULT_SUB_AGENT_TIMEOUT,
            "sub1",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("sub1 at depth 1");
    assert_eq!(sub1.sub_agent_depth, 1);
    assert_eq!(sub1.correlation_id, parent.correlation_id);

    // 3. sub2 invoked by sub1 (depth 2).
    let sub2 = runner
        .run_sub_agent(
            &sub1.run_id,
            &sub1.correlation_id,
            2,
            DEFAULT_SUB_AGENT_TIMEOUT,
            "sub2",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("sub2 at depth 2");
    assert_eq!(sub2.sub_agent_depth, 2);
    assert_eq!(sub2.correlation_id, parent.correlation_id);

    // 4. sub3 invoked by sub2 (depth 3 — the boundary).
    let sub3 = runner
        .run_sub_agent(
            &sub2.run_id,
            &sub2.correlation_id,
            MAX_SUB_AGENT_DEPTH,
            DEFAULT_SUB_AGENT_TIMEOUT,
            "sub3",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect("sub3 at MAX_SUB_AGENT_DEPTH");
    assert_eq!(sub3.sub_agent_depth, MAX_SUB_AGENT_DEPTH);
    assert_eq!(sub3.correlation_id, parent.correlation_id);

    // ── ASSERTION 1: correlation_id is the same across all 4 runs.
    assert_eq!(parent.correlation_id, sub1.correlation_id);
    assert_eq!(parent.correlation_id, sub2.correlation_id);
    assert_eq!(parent.correlation_id, sub3.correlation_id);

    // Hold the lock only for the synchronous DB queries — wrap in
    // a scope so clippy's await-holding-lock can prove the guard
    // is released before any subsequent .await.
    {
        let conn = sqlite.lock().unwrap();

        // ── ASSERTION 2: agent_runs.parent_run_id chains correctly.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_runs WHERE correlation_id = ?1",
                rusqlite::params![parent.correlation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 4, "all 4 runs land on the same correlation");

        let parent_db: Option<String> = conn
            .query_row(
                "SELECT parent_run_id FROM agent_runs WHERE id = ?1",
                rusqlite::params![parent.run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(parent_db.is_none(), "top-level run has NULL parent_run_id");

        for (run_id, expected_parent) in [
            (&sub1.run_id, &parent.run_id),
            (&sub2.run_id, &sub1.run_id),
            (&sub3.run_id, &sub2.run_id),
        ] {
            let actual: Option<String> = conn
                .query_row(
                    "SELECT parent_run_id FROM agent_runs WHERE id = ?1",
                    rusqlite::params![run_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                actual.as_deref(),
                Some(expected_parent.as_str()),
                "agent_runs.parent_run_id chains parent → sub1 → sub2 → sub3"
            );
        }

        // ── ASSERTION 3: agent_actions.parent_run_id points at the
        //    immediate parent of each sub.
        for (sub, expected_parent) in [
            (&sub1, &parent.run_id),
            (&sub2, &sub1.run_id),
            (&sub3, &sub2.run_id),
        ] {
            let action_id = sub.action_id.as_ref().expect("sub-AutoLand recorded");
            let stored_parent: Option<String> = conn
                .query_row(
                    "SELECT parent_run_id FROM agent_actions WHERE id = ?1",
                    rusqlite::params![action_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                stored_parent.as_deref(),
                Some(expected_parent.as_str()),
                "agent_actions.parent_run_id for {} → {}",
                sub.agent,
                expected_parent
            );
        }
    }

    // ── ASSERTION 4: depth past MAX is rejected before any DB write.
    let provider_count_before: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r.get(0))
            .unwrap()
    };
    let err = runner
        .run_sub_agent(
            &sub3.run_id,
            &sub3.correlation_id,
            MAX_SUB_AGENT_DEPTH + 1,
            DEFAULT_SUB_AGENT_TIMEOUT,
            "sub3",
            TriggerContext::OnDemand { note_id: None },
        )
        .await
        .expect_err("depth past MAX must error");
    assert!(
        matches!(err, RunnerError::RecursionLimitExceeded { .. }),
        "expected RecursionLimitExceeded, got {err:?}"
    );
    let provider_count_after: i64 = {
        let conn = sqlite.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM agent_runs", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(
        provider_count_after, provider_count_before,
        "over-depth call must not write an agent_runs row"
    );

    // Silence the unused-AUTOLAND_RESPONSE clippy nudge if it ever
    // fires — kept for ergonomic ad-hoc tests that don't need
    // varied paths per call.
    let _ = AUTOLAND_RESPONSE;
}
