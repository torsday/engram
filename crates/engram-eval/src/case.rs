//! YAML case fixture: schema + loader.
//!
//! The schema mirrors `01-agents-and-council.md` §Case fixture
//! format. `Case::load_dir` returns every `<id>.yaml` file in a
//! directory, sorted by file name so eval runs are deterministic.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One per-case fixture.
///
/// Maps 1:1 to the YAML in `01-agents-and-council.md`. Unknown
/// top-level keys are accepted silently via `serde(default)` on the
/// nested struct fields — the runtime tolerates schema additions so
/// older agents' case files don't need a bulk migration when the
/// format grows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    /// Stable case identifier (e.g. `001-obvious-link`). Matches the
    /// filename's basename minus the `.yaml` extension by convention,
    /// but the loader doesn't enforce equality.
    pub id: String,
    /// Human-readable case purpose. Surfaces in scorecards.
    #[serde(default)]
    pub description: String,
    /// Inputs the runner feeds into the agent invocation.
    pub input: CaseInput,
    /// What the runner expects the agent to produce. Each field is
    /// optional so a case can assert only the dimensions it cares
    /// about (e.g. a "no proposal expected" case sets only
    /// `proposes_link: false`).
    #[serde(default)]
    pub expected: ExpectedOutcome,
    /// Per-dimension weights used when computing the aggregate
    /// case score. Defaults to equal weights (1.0) for
    /// precision/recall and lower weights (0.5/0.2) for calibration
    /// and cost per the spec's reference example.
    #[serde(default)]
    pub scoring: ScoringWeights,
}

/// Vault snapshot + trigger pointers the runner uses to seed the
/// case before invoking the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseInput {
    /// Path (relative to the agent's eval dir, or absolute) to the
    /// vault snapshot tarball or directory the runner will unpack
    /// before running the case. The exact unpack mechanism is the
    /// future runner's concern; this slice just records the pointer.
    pub vault_state: String,
    /// ULID of the note the runner should invoke the agent against,
    /// when the agent's trigger is `OnDemand`/`FileChange`. `None`
    /// for cron-triggered cases.
    #[serde(default)]
    pub trigger_note_id: Option<String>,
}

/// What the case asserts about the agent's output.
///
/// Each field is optional; the runner skips dimensions a case
/// doesn't set. A case can simultaneously assert "produces a link"
/// AND a minimum confidence AND specific rationale keywords — the
/// runner ANDs the dimensions when computing pass/fail.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExpectedOutcome {
    /// True when the case expects the agent to propose at least one
    /// link; false when the case expects no proposal at all.
    #[serde(default)]
    pub proposes_link: Option<bool>,
    /// Specific link target the agent must propose. When set, the
    /// runner requires this exact `target_id` among the proposals.
    #[serde(default)]
    pub target_id: Option<String>,
    /// Floor on the agent's self-reported `confidence` field. The
    /// runner fails the case if confidence is present and below
    /// this value, or absent when this value is set.
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Ceiling on the agent's `confidence`. Useful for asserting
    /// "the agent should NOT be very confident here" cases.
    #[serde(default)]
    pub max_confidence: Option<f64>,
    /// Keywords the rationale must contain (case-insensitive). The
    /// runner requires every keyword in the list to appear.
    #[serde(default)]
    pub rationale_must_mention: Vec<String>,
}

/// Per-dimension weights for the aggregate case score.
///
/// All weights default to the spec's reference values so a case
/// fixture that omits the `scoring:` block still scores sanely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Weight applied to precision (did the agent produce only the
    /// expected actions?).
    #[serde(default = "default_one")]
    pub precision_weight: f64,
    /// Weight applied to recall (did the agent produce the expected
    /// actions?).
    #[serde(default = "default_one")]
    pub recall_weight: f64,
    /// Weight applied to calibration error (was the claimed
    /// confidence within `min_confidence..max_confidence`?).
    #[serde(default = "default_half")]
    pub calibration_weight: f64,
    /// Weight applied to cost-per-proposal (lower is better).
    #[serde(default = "default_fifth")]
    pub cost_weight: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            precision_weight: 1.0,
            recall_weight: 1.0,
            calibration_weight: 0.5,
            cost_weight: 0.2,
        }
    }
}

fn default_one() -> f64 {
    1.0
}
fn default_half() -> f64 {
    0.5
}
fn default_fifth() -> f64 {
    0.2
}

/// Errors from case loading.
#[derive(Debug, Error)]
pub enum CaseError {
    /// The cases directory could not be enumerated.
    #[error("failed to read case directory {path}: {source}")]
    DirRead {
        /// Directory that couldn't be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A specific case YAML file failed to read.
    #[error("failed to read case file {path}: {source}")]
    FileRead {
        /// File that couldn't be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A specific case YAML file failed to parse.
    #[error("failed to parse case file {path}: {source}")]
    Parse {
        /// File that didn't parse.
        path: PathBuf,
        /// Underlying YAML parse error.
        #[source]
        source: serde_yaml::Error,
    },
}

impl Case {
    /// Parse a `Case` from raw YAML.
    pub fn from_yaml(s: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(s)
    }

    /// Load every `*.yaml` (and `*.yml`) file in `dir` as a `Case`,
    /// sorted by file name so eval runs are deterministic. Returns
    /// an empty Vec if the directory exists but has no case files.
    ///
    /// Non-YAML files (e.g. `README.md`, hidden files) are silently
    /// skipped. Subdirectories are not recursed — flat layout only.
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>, CaseError> {
        let entries = std::fs::read_dir(dir).map_err(|e| CaseError::DirRead {
            path: dir.to_path_buf(),
            source: e,
        })?;

        // Collect (path, filename) pairs so we can sort deterministically
        // before parsing — parse errors should surface in a predictable
        // order rather than depending on filesystem enumeration order.
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| CaseError::DirRead {
                path: dir.to_path_buf(),
                source: e,
            })?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            match path.extension().and_then(|s| s.to_str()) {
                Some("yaml") | Some("yml") => paths.push(path),
                _ => {}
            }
        }
        paths.sort();

        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let body = std::fs::read_to_string(&path).map_err(|e| CaseError::FileRead {
                path: path.clone(),
                source: e,
            })?;
            let case = Case::from_yaml(&body).map_err(|e| CaseError::Parse {
                path: path.clone(),
                source: e,
            })?;
            out.push(case);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const REFERENCE_CASE: &str = r#"
id: 001-obvious-link
description: Two notes share a concept; expect a link.
input:
  vault_state: snapshot/cases/001/vault.tar
  trigger_note_id: 01JRZK3M7P
expected:
  proposes_link: true
  target_id: 01JRZK4N8Q
  min_confidence: 0.85
  rationale_must_mention:
    - semantic
    - agreement
scoring:
  precision_weight: 1.0
  recall_weight: 1.0
  calibration_weight: 0.5
  cost_weight: 0.2
"#;

    #[test]
    fn reference_case_round_trips_verbatim() {
        let case = Case::from_yaml(REFERENCE_CASE).unwrap();
        assert_eq!(case.id, "001-obvious-link");
        assert_eq!(case.input.vault_state, "snapshot/cases/001/vault.tar");
        assert_eq!(case.input.trigger_note_id.as_deref(), Some("01JRZK3M7P"));
        assert_eq!(case.expected.proposes_link, Some(true));
        assert_eq!(case.expected.target_id.as_deref(), Some("01JRZK4N8Q"));
        assert_eq!(case.expected.min_confidence, Some(0.85));
        assert_eq!(
            case.expected.rationale_must_mention,
            vec!["semantic".to_string(), "agreement".to_string()]
        );
        assert_eq!(case.scoring.precision_weight, 1.0);
        assert_eq!(case.scoring.calibration_weight, 0.5);
        assert_eq!(case.scoring.cost_weight, 0.2);
    }

    #[test]
    fn missing_optional_blocks_use_defaults() {
        let minimal = r#"
id: 002-minimal
input:
  vault_state: snapshot/cases/002/vault.tar
"#;
        let case = Case::from_yaml(minimal).unwrap();
        assert_eq!(case.id, "002-minimal");
        assert!(case.description.is_empty());
        assert!(case.expected.proposes_link.is_none());
        assert!(case.expected.target_id.is_none());
        assert!(case.expected.rationale_must_mention.is_empty());
        // Scoring defaults to the spec's reference weights.
        assert_eq!(case.scoring, ScoringWeights::default());
    }

    #[test]
    fn load_dir_returns_cases_sorted_by_filename() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("003-z.yaml"),
            r#"
id: 003-zebra
input:
  vault_state: v
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("001-a.yaml"),
            r#"
id: 001-alpha
input:
  vault_state: v
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("002-m.yml"),
            r#"
id: 002-middle
input:
  vault_state: v
"#,
        )
        .unwrap();
        // A non-YAML file is silently skipped.
        std::fs::write(dir.path().join("README.md"), "not a case").unwrap();

        let cases = Case::load_dir(dir.path()).unwrap();
        let ids: Vec<&str> = cases.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["001-alpha", "002-middle", "003-zebra"]);
    }

    #[test]
    fn load_dir_empty_directory_yields_empty_vec() {
        let dir = tempdir().unwrap();
        let cases = Case::load_dir(dir.path()).unwrap();
        assert!(cases.is_empty());
    }

    #[test]
    fn load_dir_missing_directory_surfaces_dir_read_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        match Case::load_dir(&missing) {
            Err(CaseError::DirRead { path, .. }) => assert_eq!(path, missing),
            other => panic!("expected DirRead error, got {other:?}"),
        }
    }

    #[test]
    fn load_dir_malformed_yaml_surfaces_parse_error_with_path() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("004-bad.yaml");
        std::fs::write(&bad, "id: : :\n  invalid: yaml: : :").unwrap();
        match Case::load_dir(dir.path()) {
            Err(CaseError::Parse { path, .. }) => assert_eq!(path, bad),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_keys_are_rejected_strictly() {
        // serde_yaml is strict by default — a future schema slice
        // that adds a new top-level block must either land both
        // here and in the fixtures, or set `serde(deny_unknown_fields)
        // = false`. This test pins the current behavior so we
        // notice if it changes silently.
        let with_extra = r#"
id: 005-extra
input:
  vault_state: v
future_field: 42
"#;
        // Default serde behavior is to ignore unknown fields, which
        // is what we want for forward compatibility. Confirm the
        // case still parses cleanly.
        let case = Case::from_yaml(with_extra).expect("unknown fields must be tolerated");
        assert_eq!(case.id, "005-extra");
    }
}
