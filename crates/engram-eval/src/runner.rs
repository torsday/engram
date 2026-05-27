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
#[derive(Debug, Clone, PartialEq)]
pub struct EvalRunReport {
    /// Agent the run targeted (echoed from `EvalRunner::new`).
    pub agent: String,
    /// Per-case results in fixture order.
    pub results: Vec<CaseRunResult>,
    /// Aggregate metrics over `results`.
    pub aggregate: Aggregate,
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
        }
    }
}

/// Build a uniform `CaseRunResult` for the error path so the
/// runner always returns one row per case regardless of failure.
fn error_result(case: &Case, _detail: String) -> CaseRunResult {
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
}
