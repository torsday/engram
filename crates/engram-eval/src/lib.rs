//! Agent evaluation framework — case fixtures, scoring primitives,
//! scorecard regeneration, and CI integration per issue #39.
//!
//! This slice covers the **data layer**: the YAML case fixture
//! format and its loader, plus the [`Score`] / [`Verdict`] value
//! types every per-case run produces. The runner that walks cases
//! against a live agent, the scorecard markdown emitter, and the CI
//! gate are follow-up slices that build on the types here.
//!
//! # Case fixture format
//!
//! Per `docs/design/01-agents-and-council.md` §Case fixture format,
//! a `.engram/evals/<agent>/cases/<id>.yaml` looks like:
//!
//! ```yaml
//! id: 001-obvious-link
//! description: Two notes share an obvious concept; expect a link.
//! input:
//!   vault_state: snapshot/cases/001/vault.tar
//!   trigger_note_id: 01JRZK3M7P...
//! expected:
//!   proposes_link: true
//!   target_id: 01JRZK4N8Q...
//!   min_confidence: 0.85
//!   rationale_must_mention: ["semantic", "agreement"]
//! scoring:
//!   precision_weight: 1.0
//!   recall_weight: 1.0
//!   calibration_weight: 0.5
//!   cost_weight: 0.2
//! ```
//!
//! The loader is permissive about unknown keys (via `serde(default)`
//! / `serde(other)` where appropriate) so future schema additions
//! don't reject existing fixtures.

/// Per-case fixture loaded from `.engram/evals/<agent>/cases/<id>.yaml`.
pub mod case;
/// Score / Verdict value types — what one case run produces.
pub mod score;

pub use case::{Case, CaseError, CaseInput, ExpectedOutcome, ScoringWeights};
pub use score::{Score, Verdict};
