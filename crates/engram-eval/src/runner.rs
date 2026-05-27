//! Eval runner — the orchestration layer that walks a directory
//! of cases, seeds each one's vault from the snapshot cache,
//! invokes an agent through a caller-provided closure, scores the
//! observation, and aggregates.
//!
//! This slice deliberately keeps the agent invocation behind a
//! `Box<dyn Fn>` so the runner stays decoupled from
//! `engram_agents::AgentRunner` for unit testing and so a future
//! slice can wire the real production invoker without touching
//! anything in this file. The companion AC bullets
//! (`agent_prompt_sha`, `agent_config_sha`, eval_runs DB
//! persistence, JSON artifact under `.engram/evals/…`) land in
//! later slices.

use std::path::{Path, PathBuf};

use crate::aggregate::{Aggregate, CaseRunResult};
use crate::case::{Case, CaseError};
use crate::scorer::{score_case, Observation};
use crate::snapshot::SnapshotError;
use crate::SnapshotCache;
use crate::Verdict;

/// Closure signature for invoking the agent against an unpacked
/// vault. Receives the case (so the invoker can use
/// `case.input.trigger_note_id`, the agent name from context,
/// etc.) and the path to the seeded vault directory.
///
/// `Box<dyn Fn>` rather than a generic so the runner stays object-
/// safe and so tests can swap the invoker out without changing
/// `EvalRunner` generics.
pub type Invoker = Box<dyn Fn(&Case, &Path) -> Result<Observation, InvokerError> + Send + Sync>;

/// Wraps any error the invoker surfaces. The runner converts these
/// to `Verdict::Error` per-case so a single broken invocation
/// doesn't abort the run — every case still produces a row.
#[derive(Debug)]
pub struct InvokerError {
    /// Human-readable message; appears in the report's `note` field.
    pub message: String,
}

impl InvokerError {
    /// Convenience constructor.
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for InvokerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invoker error: {}", self.message)
    }
}

impl std::error::Error for InvokerError {}

/// One eval run's full report.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvalRunReport {
    /// Agent the run targeted (echoed from `EvalRunner::new`).
    pub agent: String,
    /// Per-case results in fixture order.
    pub results: Vec<CaseRunResult>,
    /// Aggregate metrics over `results`.
    pub aggregate: Aggregate,
}

/// Caller-supplied metadata for persisting an [`EvalRunReport`]
/// to the `eval_runs` table.
///
/// `agent_prompt_sha` / `agent_config_sha` are SHA-256 hashes of
/// the agent's `prompt.md` / `config.toml` at run time — the eval
/// framework uses them to gate "prompt-evolution variants must
/// beat the active prompt." Computing these is the
/// AgentRunner-adapter slice's job; this slice just persists what
/// the caller provides.
#[derive(Debug, Clone)]
pub struct PersistParams<'a> {
    /// SHA-256 of the agent's prompt.md at run time.
    pub agent_prompt_sha: &'a str,
    /// SHA-256 of the agent's config.toml at run time.
    pub agent_config_sha: &'a str,
    /// Model the run invoked (e.g. `"claude-3-5-haiku-20250901"`).
    pub model_used: &'a str,
    /// Path to the JSON artifact this run wrote (or will write).
    /// Stored verbatim in `eval_runs.output_path`.
    pub output_path: &'a std::path::Path,
    /// RFC3339 start time of the run.
    pub started_at: &'a str,
    /// RFC3339 completion time of the run.
    pub completed_at: &'a str,
    /// Total tokens consumed across every case. Caller computes
    /// from the per-case Observations (the runner doesn't see
    /// token counts directly — that's the invoker's province).
    pub total_tokens: i64,
}

impl EvalRunReport {
    /// Write a JSON artifact of this report under `runs_dir`.
    ///
    /// Creates `runs_dir` (with parents) if missing. Filename is
    /// `<RFC3339-utc>-<agent>.json` so multiple runs of the same
    /// agent on different days sort chronologically by `ls`. The
    /// `:` and `.` in the timestamp are replaced with `-` so the
    /// filename is portable across filesystems (FAT/NTFS forbid
    /// `:`). The agent name is sanitized to `[A-Za-z0-9-]` with
    /// other characters replaced by `_`.
    ///
    /// Returns the absolute path written. Pure I/O wrapper around
    /// `serde_json::to_string_pretty` — no DB writes. The
    /// `eval_runs` / `eval_case_results` persistence is a separate
    /// slice.
    pub fn write_json(&self, runs_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(runs_dir)?;
        let stamp = chrono::Utc::now()
            .format("%Y-%m-%dT%H-%M-%S-%fZ")
            .to_string();
        let safe_agent: String = self
            .agent
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let filename = format!("{stamp}-{safe_agent}.json");
        let path = runs_dir.join(filename);
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, body)?;
        Ok(path)
    }

    /// Persist this report to SQLite per the `eval_runs` /
    /// `eval_case_results` schema from `001_initial.sql`.
    ///
    /// Generates a fresh ULID for the run, INSERTs the `eval_runs`
    /// row, then one `eval_case_results` row per case — all inside
    /// a single transaction so partial writes never appear. The
    /// returned `String` is the new run id, also the FK in every
    /// case_results row.
    ///
    /// `aggregate_metrics` is stored as a JSON-serialized
    /// [`Aggregate`] so the row remains stable even when the
    /// in-memory struct grows new fields. `scores` per case is the
    /// same: JSON-serialized [`Score`].
    pub fn persist(
        &self,
        conn: &mut rusqlite::Connection,
        params: &PersistParams<'_>,
    ) -> rusqlite::Result<String> {
        let run_id = ulid::Ulid::new().to_string();
        let aggregate_json = serde_json::to_string(&self.aggregate).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                e,
            )))
        })?;
        let total_cost_usd: f64 = self.results.iter().map(|r| r.score.cost_usd).sum();
        let cases_run = self.aggregate.total_cases as i64;
        let cases_passed = self.aggregate.passed as i64;

        let txn = conn.transaction()?;
        txn.execute(
            "INSERT INTO eval_runs ( \
                id, agent, started_at, completed_at, \
                agent_prompt_sha, agent_config_sha, model_used, \
                cases_run, cases_passed, total_tokens, total_cost_usd, \
                aggregate_metrics, output_path \
              ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                run_id,
                self.agent,
                params.started_at,
                params.completed_at,
                params.agent_prompt_sha,
                params.agent_config_sha,
                params.model_used,
                cases_run,
                cases_passed,
                params.total_tokens,
                total_cost_usd,
                aggregate_json,
                params.output_path.to_string_lossy().into_owned(),
            ],
        )?;

        for r in &self.results {
            let scores_json = serde_json::to_string(&r.score).map_err(|e| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e,
                )))
            })?;
            // failure_reason is the runner-captured detail string
            // on Verdict::Error (slice 12), None otherwise.
            txn.execute(
                "INSERT INTO eval_case_results ( \
                    eval_run_id, case_id, result, scores, failure_reason \
                  ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    run_id,
                    r.case_id,
                    r.verdict.as_sql(),
                    scores_json,
                    r.failure_reason
                ],
            )?;
        }
        txn.commit()?;
        Ok(run_id)
    }
}

/// Errors `EvalRunner` itself surfaces (distinct from per-case
/// invoker errors, which become `Verdict::Error` rows).
#[derive(Debug, thiserror::Error)]
pub enum EvalRunnerError {
    /// Loading the case fixtures failed.
    #[error("case load failed: {0}")]
    CaseLoad(#[from] CaseError),
    /// Snapshot unpacking / cache failed.
    #[error("snapshot failed: {0}")]
    Snapshot(#[from] SnapshotError),
    /// `run_subset` was passed a case_id that the loader didn't
    /// return.
    #[error("case_id `{0}` not found in cases dir")]
    UnknownCase(String),
}

/// Walks a directory of cases and runs each against an agent.
///
/// Construct with [`EvalRunner::new`], then call [`run_all`] or
/// [`run_subset`]. The closure-based invoker lets this slice stay
/// decoupled from `engram_agents`; a future slice will provide an
/// `AgentRunner`-backed adapter.
pub struct EvalRunner {
    agent: String,
    cases_dir: PathBuf,
    cache: SnapshotCache,
    invoker: Invoker,
}

impl EvalRunner {
    /// `agent` names the agent under evaluation (used in
    /// `EvalRunReport.agent` and in the future scorecard markdown).
    /// `cases_dir` points at `.engram/evals/<agent>/cases/`.
    /// `cache` keys the unpacked vault snapshots. `invoker`
    /// performs the actual agent run; the runner gives it the case
    /// and the path to the seeded vault, expects an [`Observation`]
    /// back.
    pub fn new(
        agent: impl Into<String>,
        cases_dir: impl Into<PathBuf>,
        cache: SnapshotCache,
        invoker: Invoker,
    ) -> Self {
        Self {
            agent: agent.into(),
            cases_dir: cases_dir.into(),
            cache,
            invoker,
        }
    }

    /// Load every case fixture in `cases_dir` (deterministic order
    /// per [`Case::load_dir`]).
    pub fn load_cases(&self) -> Result<Vec<Case>, EvalRunnerError> {
        Ok(Case::load_dir(&self.cases_dir)?)
    }

    /// Run every case in `cases_dir`. Each case unpacks its
    /// snapshot via the cache, then invokes the closure. Invoker
    /// errors become per-case `Verdict::Error` rows — they don't
    /// abort the run.
    pub fn run_all(&self) -> Result<EvalRunReport, EvalRunnerError> {
        let cases = self.load_cases()?;
        self.run_cases(&cases)
    }

    /// Run only the cases whose `id` appears in `ids`. Order in the
    /// report mirrors `ids`. Unknown ids return `UnknownCase`.
    pub fn run_subset(&self, ids: &[&str]) -> Result<EvalRunReport, EvalRunnerError> {
        let all = self.load_cases()?;
        let mut selected = Vec::with_capacity(ids.len());
        for id in ids {
            let case = all
                .iter()
                .find(|c| c.id == *id)
                .ok_or_else(|| EvalRunnerError::UnknownCase((*id).to_string()))?;
            selected.push(case.clone());
        }
        self.run_cases(&selected)
    }

    fn run_cases(&self, cases: &[Case]) -> Result<EvalRunReport, EvalRunnerError> {
        let mut results = Vec::with_capacity(cases.len());
        for case in cases {
            let result = self.run_one(case);
            results.push(result);
        }
        let aggregate = Aggregate::from_results(&results);
        Ok(EvalRunReport {
            agent: self.agent.clone(),
            results,
            aggregate,
        })
    }

    fn run_one(&self, case: &Case) -> CaseRunResult {
        // The vault_state pointer is interpreted relative to the
        // cases_dir when relative — case fixtures live next to
        // their snapshots in `.engram/evals/<agent>/`.
        let src = Path::new(&case.input.vault_state);
        let src_resolved: PathBuf = if src.is_absolute() {
            src.to_path_buf()
        } else {
            self.cases_dir.join(src)
        };

        let vault_path = match self.cache.ensure_unpacked(&src_resolved) {
            Ok(p) => p,
            Err(e) => {
                // Snapshot failure → Error verdict + zero score.
                return error_result(case, format!("snapshot unpack failed: {e}"));
            }
        };

        let observation = match (self.invoker)(case, &vault_path) {
            Ok(obs) => obs,
            Err(e) => return error_result(case, e.message),
        };

        let proposals_emitted = observation.proposed_link_targets.len();
        let (score, verdict) = score_case(&case.expected, &observation);
        CaseRunResult {
            case_id: case.id.clone(),
            verdict,
            score,
            proposals_emitted,
            failure_reason: None,
        }
    }
}

/// Build a uniform `CaseRunResult` for the error path so the
/// runner always returns one row per case regardless of failure.
fn error_result(case: &Case, detail: String) -> CaseRunResult {
    CaseRunResult {
        case_id: case.id.clone(),
        verdict: Verdict::Error,
        score: crate::Score {
            precision: 0.0,
            recall: 0.0,
            calibration: 0.0,
            cost: 1.0,
            cost_usd: 0.0,
        },
        proposals_emitted: 0,
        failure_reason: Some(detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_case(dir: &Path, id: &str, vault_state: &str, expected_target: Option<&str>) {
        let body = match expected_target {
            Some(t) => format!(
                "id: {id}\ninput:\n  vault_state: {vault_state}\nexpected:\n  proposes_link: true\n  target_id: {t}\n"
            ),
            None => format!(
                "id: {id}\ninput:\n  vault_state: {vault_state}\nexpected:\n  proposes_link: false\n"
            ),
        };
        std::fs::write(dir.join(format!("{id}.yaml")), body).unwrap();
    }

    fn write_vault(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("seed.md"), "hi").unwrap();
    }

    fn perfect_invoker(target: &'static str) -> Invoker {
        Box::new(move |case, _vault| {
            Ok(Observation {
                proposed_link_targets: vec![target.into()],
                confidence: Some(0.99),
                rationale: Some(format!("invoked for {}", case.id)),
                cost_usd: 0.0,
            })
        })
    }

    #[test]
    fn run_all_produces_one_result_per_case_in_fixture_order() {
        let dir = tempdir().unwrap();
        // Two cases pointing at the same vault — exercise cache reuse.
        let vault = dir.path().join("vault");
        write_vault(&vault);
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        write_case(
            &cases_dir,
            "001-a",
            vault.to_str().unwrap(),
            Some("01TARGET"),
        );
        write_case(
            &cases_dir,
            "002-b",
            vault.to_str().unwrap(),
            Some("01TARGET"),
        );

        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("linker", &cases_dir, cache, perfect_invoker("01TARGET"));

        let report = runner.run_all().unwrap();
        assert_eq!(report.agent, "linker");
        let ids: Vec<&str> = report.results.iter().map(|r| r.case_id.as_str()).collect();
        assert_eq!(ids, vec!["001-a", "002-b"]);
        assert_eq!(report.aggregate.total_cases, 2);
        assert_eq!(report.aggregate.passed, 2);
        assert_eq!(report.aggregate.pass_rate, 1.0);
    }

    #[test]
    fn run_subset_returns_only_requested_cases_in_request_order() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        write_vault(&vault);
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        write_case(&cases_dir, "001-a", vault.to_str().unwrap(), Some("01T"));
        write_case(&cases_dir, "002-b", vault.to_str().unwrap(), Some("01T"));
        write_case(&cases_dir, "003-c", vault.to_str().unwrap(), Some("01T"));

        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("linker", &cases_dir, cache, perfect_invoker("01T"));

        let report = runner.run_subset(&["003-c", "001-a"]).unwrap();
        let ids: Vec<&str> = report.results.iter().map(|r| r.case_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["003-c", "001-a"],
            "subset preserves caller's order"
        );
    }

    #[test]
    fn run_subset_unknown_id_returns_error() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        write_vault(&vault);
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        write_case(&cases_dir, "001-only", vault.to_str().unwrap(), None);
        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("any", &cases_dir, cache, perfect_invoker("x"));
        match runner.run_subset(&["nope"]) {
            Err(EvalRunnerError::UnknownCase(id)) => assert_eq!(id, "nope"),
            other => panic!("expected UnknownCase, got {other:?}"),
        }
    }

    #[test]
    fn invoker_error_becomes_error_verdict_not_aborted_run() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        write_vault(&vault);
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        write_case(&cases_dir, "001-a", vault.to_str().unwrap(), Some("01T"));
        write_case(&cases_dir, "002-b", vault.to_str().unwrap(), Some("01T"));

        let cache = SnapshotCache::new(tempdir().unwrap().path());
        // Invoker errors on every case.
        let invoker: Invoker =
            Box::new(|case, _v| Err(InvokerError::new(format!("simulated panic in {}", case.id))));
        let runner = EvalRunner::new("linker", &cases_dir, cache, invoker);
        let report = runner.run_all().unwrap();

        assert_eq!(report.results.len(), 2, "every case still produces a row");
        for r in &report.results {
            assert_eq!(r.verdict, Verdict::Error);
        }
        assert_eq!(report.aggregate.errored, 2);
        assert_eq!(report.aggregate.passed, 0);
    }

    #[test]
    fn missing_snapshot_source_becomes_error_verdict() {
        let dir = tempdir().unwrap();
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        // vault_state points at a path that doesn't exist.
        write_case(&cases_dir, "001-missing", "no-such-vault", None);

        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("any", &cases_dir, cache, perfect_invoker("x"));
        let report = runner.run_all().unwrap();
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].verdict, Verdict::Error);
    }

    #[test]
    fn vault_state_relative_path_resolves_under_cases_dir() {
        let dir = tempdir().unwrap();
        // Vault lives inside the cases dir as a peer dir.
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        let vault = cases_dir.join("vault_001");
        write_vault(&vault);
        // vault_state is RELATIVE — runner must resolve it under
        // cases_dir.
        write_case(&cases_dir, "001-rel", "vault_001", Some("01T"));

        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("any", &cases_dir, cache, perfect_invoker("01T"));
        let report = runner.run_all().unwrap();
        assert_eq!(report.results[0].verdict, Verdict::Pass);
    }

    #[test]
    fn invoker_receives_unpacked_vault_path() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        write_vault(&vault);
        std::fs::write(vault.join("marker.md"), "hello-from-vault").unwrap();
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        write_case(
            &cases_dir,
            "001-marker",
            vault.to_str().unwrap(),
            Some("01T"),
        );

        // Invoker reads the seeded vault to prove the path is real.
        let invoker: Invoker = Box::new(|_case, vault_path| {
            let marker = std::fs::read_to_string(vault_path.join("marker.md"))
                .map_err(|e| InvokerError::new(format!("invoker couldn't find marker.md: {e}")))?;
            assert_eq!(marker, "hello-from-vault");
            Ok(Observation {
                proposed_link_targets: vec!["01T".into()],
                confidence: Some(0.99),
                ..Default::default()
            })
        });
        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("linker", &cases_dir, cache, invoker);
        let report = runner.run_all().unwrap();
        assert_eq!(report.results[0].verdict, Verdict::Pass);
    }

    fn empty_report(agent: &str) -> EvalRunReport {
        EvalRunReport {
            agent: agent.into(),
            results: Vec::new(),
            aggregate: Aggregate::empty(),
        }
    }

    #[test]
    fn write_json_creates_runs_dir_and_returns_path() {
        let parent = tempdir().unwrap();
        let runs_dir = parent.path().join("runs/missing/parents");
        assert!(!runs_dir.exists());
        let path = empty_report("linker").write_json(&runs_dir).unwrap();
        assert!(path.exists());
        assert!(path.starts_with(&runs_dir));
    }

    #[test]
    fn write_json_filename_starts_with_iso_stamp_and_ends_with_agent_json() {
        let parent = tempdir().unwrap();
        let path = empty_report("linker").write_json(parent.path()).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // YYYY-MM-DDTHH-MM-SS-...
        assert!(name.starts_with(&chrono::Utc::now().format("%Y-").to_string()));
        assert!(name.ends_with("-linker.json"));
    }

    #[test]
    fn write_json_sanitizes_disallowed_filename_characters() {
        let parent = tempdir().unwrap();
        let path = empty_report("evil/agent name?")
            .write_json(parent.path())
            .unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.ends_with("-evil_agent_name_.json"), "got {name}");
    }

    #[test]
    fn write_json_round_trips_through_serde() {
        let parent = tempdir().unwrap();
        let report = EvalRunReport {
            agent: "scribe".into(),
            results: vec![CaseRunResult {
                case_id: "001".into(),
                verdict: Verdict::Pass,
                score: crate::Score::perfect(),
                proposals_emitted: 0,
                failure_reason: None,
            }],
            aggregate: Aggregate::from_results(&[CaseRunResult {
                case_id: "001".into(),
                verdict: Verdict::Pass,
                score: crate::Score::perfect(),
                proposals_emitted: 0,
                failure_reason: None,
            }]),
        };
        let path = report.write_json(parent.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: EvalRunReport = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, report);
    }

    // ─── persist (DB) tests ───────────────────────────────────────

    fn setup_sqlite() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        engram_index::sqlite::Migrator::new(&conn)
            .apply_all()
            .unwrap();
        conn
    }

    fn one_case_pass() -> CaseRunResult {
        CaseRunResult {
            case_id: "001-a".into(),
            verdict: Verdict::Pass,
            score: crate::Score {
                precision: 1.0,
                recall: 1.0,
                calibration: 1.0,
                cost: 0.9,
                cost_usd: 0.10,
            },
            proposals_emitted: 1,
            failure_reason: None,
        }
    }

    fn one_case_fail() -> CaseRunResult {
        CaseRunResult {
            case_id: "002-b".into(),
            verdict: Verdict::Fail,
            score: crate::Score {
                precision: 0.5,
                recall: 0.0,
                calibration: 1.0,
                cost: 0.95,
                cost_usd: 0.05,
            },
            proposals_emitted: 2,
            failure_reason: None,
        }
    }

    #[test]
    fn persist_inserts_run_row_and_returns_ulid() {
        let mut conn = setup_sqlite();
        let report = EvalRunReport {
            agent: "linker".into(),
            results: vec![one_case_pass()],
            aggregate: Aggregate::from_results(&[one_case_pass()]),
        };
        let path = std::path::PathBuf::from("/tmp/run.json");
        let params = PersistParams {
            agent_prompt_sha: "promptsha",
            agent_config_sha: "configsha",
            model_used: "claude-3-5-haiku",
            output_path: &path,
            started_at: "2026-05-27T00:00:00Z",
            completed_at: "2026-05-27T00:00:10Z",
            total_tokens: 1234,
        };
        let run_id = report.persist(&mut conn, &params).unwrap();
        assert_eq!(run_id.len(), 26, "ULID is 26 chars");

        let (agent, cases_run, cases_passed, total_tokens, total_cost): (
            String,
            i64,
            i64,
            i64,
            f64,
        ) = conn
            .query_row(
                "SELECT agent, cases_run, cases_passed, total_tokens, total_cost_usd \
                 FROM eval_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(agent, "linker");
        assert_eq!(cases_run, 1);
        assert_eq!(cases_passed, 1);
        assert_eq!(total_tokens, 1234);
        assert!((total_cost - 0.10).abs() < 1e-9);
    }

    #[test]
    fn persist_writes_one_case_results_row_per_case() {
        let mut conn = setup_sqlite();
        let results = vec![one_case_pass(), one_case_fail()];
        let report = EvalRunReport {
            agent: "linker".into(),
            aggregate: Aggregate::from_results(&results),
            results,
        };
        let params = PersistParams {
            agent_prompt_sha: "p",
            agent_config_sha: "c",
            model_used: "m",
            output_path: std::path::Path::new("/tmp/r.json"),
            started_at: "2026-05-27T00:00:00Z",
            completed_at: "2026-05-27T00:00:10Z",
            total_tokens: 100,
        };
        let run_id = report.persist(&mut conn, &params).unwrap();

        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM eval_case_results WHERE eval_run_id = ?1",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 2);

        // Verdict round-trips through as_sql.
        let pass_result: String = conn
            .query_row(
                "SELECT result FROM eval_case_results \
                 WHERE eval_run_id = ?1 AND case_id = '001-a'",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pass_result, "pass");
        let fail_result: String = conn
            .query_row(
                "SELECT result FROM eval_case_results \
                 WHERE eval_run_id = ?1 AND case_id = '002-b'",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fail_result, "fail");
    }

    #[test]
    fn persist_stores_aggregate_as_json() {
        let mut conn = setup_sqlite();
        let results = vec![one_case_pass()];
        let report = EvalRunReport {
            agent: "linker".into(),
            aggregate: Aggregate::from_results(&results),
            results,
        };
        let params = PersistParams {
            agent_prompt_sha: "p",
            agent_config_sha: "c",
            model_used: "m",
            output_path: std::path::Path::new("/tmp/r.json"),
            started_at: "2026-05-27T00:00:00Z",
            completed_at: "2026-05-27T00:00:10Z",
            total_tokens: 0,
        };
        let run_id = report.persist(&mut conn, &params).unwrap();
        let json: String = conn
            .query_row(
                "SELECT aggregate_metrics FROM eval_runs WHERE id = ?1",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: Aggregate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, report.aggregate);
    }

    #[test]
    fn persist_empty_run_writes_zero_case_rows() {
        let mut conn = setup_sqlite();
        let report = empty_report("nobody");
        let params = PersistParams {
            agent_prompt_sha: "p",
            agent_config_sha: "c",
            model_used: "m",
            output_path: std::path::Path::new("/tmp/r.json"),
            started_at: "2026-05-27T00:00:00Z",
            completed_at: "2026-05-27T00:00:10Z",
            total_tokens: 0,
        };
        let run_id = report.persist(&mut conn, &params).unwrap();
        let row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM eval_case_results WHERE eval_run_id = ?1",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 0);
        // eval_runs row still landed.
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM eval_runs WHERE id = ?1)",
                rusqlite::params![run_id],
                |r| r.get::<_, i64>(0).map(|v| v != 0),
            )
            .unwrap();
        assert!(exists);
    }

    /// Verdict::Error rows carry the runner-captured detail string
    /// in `failure_reason` — both in-memory on `CaseRunResult` and
    /// persisted into `eval_case_results.failure_reason`.
    #[test]
    fn verdict_error_persists_failure_reason() {
        let mut conn = setup_sqlite();
        // Build a hand-crafted Error result with a known message.
        let results = vec![CaseRunResult {
            case_id: "001-x".into(),
            verdict: Verdict::Error,
            score: crate::Score {
                precision: 0.0,
                recall: 0.0,
                calibration: 0.0,
                cost: 1.0,
                cost_usd: 0.0,
            },
            proposals_emitted: 0,
            failure_reason: Some("simulated invoker panic".into()),
        }];
        let report = EvalRunReport {
            agent: "linker".into(),
            aggregate: Aggregate::from_results(&results),
            results,
        };
        let params = PersistParams {
            agent_prompt_sha: "p",
            agent_config_sha: "c",
            model_used: "m",
            output_path: std::path::Path::new("/tmp/r.json"),
            started_at: "2026-05-27T00:00:00Z",
            completed_at: "2026-05-27T00:00:10Z",
            total_tokens: 0,
        };
        let run_id = report.persist(&mut conn, &params).unwrap();
        let reason: Option<String> = conn
            .query_row(
                "SELECT failure_reason FROM eval_case_results \
                 WHERE eval_run_id = ?1 AND case_id = '001-x'",
                rusqlite::params![run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason.as_deref(), Some("simulated invoker panic"));
    }

    /// Invoker-error path captures the InvokerError message verbatim
    /// onto the resulting CaseRunResult.failure_reason.
    #[test]
    fn runner_captures_invoker_message_into_failure_reason() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("seed.md"), "hi").unwrap();
        let cases_dir = dir.path().join("cases");
        std::fs::create_dir_all(&cases_dir).unwrap();
        std::fs::write(
            cases_dir.join("001-broken.yaml"),
            format!(
                "id: 001-broken\ninput:\n  vault_state: {}\nexpected:\n  proposes_link: true\n",
                vault.display()
            ),
        )
        .unwrap();

        let invoker: Invoker =
            Box::new(|_case, _vault| Err(InvokerError::new("synthetic invoker failure")));
        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let runner = EvalRunner::new("linker", &cases_dir, cache, invoker);
        let report = runner.run_all().unwrap();
        assert_eq!(report.results[0].verdict, Verdict::Error);
        assert_eq!(
            report.results[0].failure_reason.as_deref(),
            Some("synthetic invoker failure")
        );
    }
}
