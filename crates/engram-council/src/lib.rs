//! Council deliberation engine.
//!
//! Implements the deliberation protocol from
//! `docs/design/01-agents-and-council.md` §The council: deliberation protocol —
//! the bounded state machine
//!
//! ```text
//! DRAFT → CRITIQUE → REVISE → CONVERGE → {LAND | PROPOSE | SHELVE}
//! ```
//!
//! that turns a proposing agent's change plus a quorum of reviewers' votes into
//! one of three terminal outcomes.
//!
//! # Scope of this slice (#34, vertical slice 1)
//!
//! This crate is the **pure decision core** of the council. It owns:
//!
//! - the [`state`] machine ([`state::Phase`], [`state::CouncilSession`]) and the
//!   bounded `CRITIQUE → REVISE → CRITIQUE` round progression;
//! - [`quorum`] selection — convening agent + opt-in participants + explicitly
//!   relevant agents, deduplicated and order-stable;
//! - the [`vote`] types ([`vote::Vote`], [`vote::VoteKind`]) and the
//!   [`converge`] tally that maps a round's votes to a [`Outcome`].
//!
//! Everything here is **synchronous and side-effect-free**: no LLM calls, no
//! disk writes, no SQLite. That is deliberate — the decision logic is the part
//! that must be exhaustively unit-tested, and keeping it pure means the tests
//! need no async runtime, no tempdir, and no mock provider.
//!
//! # Deferred to follow-ups (tracked on #34's follow-up issue)
//!
//! - **CRITIQUE/REVISE LLM rounds.** Driving the quorum's agents to actually
//!   produce votes via `engram-llm` (each critic is an independent call) is the
//!   async orchestration layer that sits *on top of* this core. The core
//!   exposes [`state::CouncilSession::record_round`] / [`state::CouncilSession::revise`]
//!   so that layer just feeds it votes.
//! - **Steelman rationality gate (#35).** Critical agents' votes must pass
//!   `SteelmanGate::evaluate` before counting. The hook is [`vote::Vote::gated`]
//!   — a vote carries whether it survived the gate; [`converge`] already ignores
//!   gate-failed votes. The gate *implementation* lands with #35.
//! - **Persistence.** Transcript markdown at `.engram/deliberations/<id>.md` and
//!   the `deliberations` / `deliberation_votes` SQLite rows. [`Outcome`] and
//!   [`state::CouncilSession`] expose everything a persistence layer needs to
//!   render them.
//! - **Wall-clock budget / timeout → SHELVE.** A scheduling concern for the
//!   async driver, not the pure core.

pub mod converge;
pub mod quorum;
pub mod state;
pub mod vote;

pub use converge::{tally, Outcome};
pub use quorum::{select_quorum, QuorumInput};
pub use state::{CouncilError, CouncilSession, Phase};
pub use vote::{Vote, VoteKind};

/// A change proposed to the council.
///
/// Mirrors the shape the proposing agent submits in the DRAFT phase: *what*
/// (the affected paths), *why* (the rationale), and how invasive the change is
/// (which steers PROPOSE vs LAND at convergence). This is intentionally a lean,
/// self-contained value type rather than a re-export of
/// `engram_agents::runner::ProposedChange` — the council core must not depend on
/// the agent runner (that would invert the eventual dependency direction: the
/// runner convenes the council, not the reverse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedChange {
    /// The agent convening the council (e.g. `"synthesizer"`). Kebab-case,
    /// matching the on-disk `agents/<name>/` directory.
    pub proposing_agent: String,
    /// One-paragraph rationale: *why* this change.
    pub rationale: String,
    /// Vault-relative paths the change touches (created, modified, or deleted).
    pub affected_paths: Vec<String>,
    /// How invasive the change is. Drives convergence: a `Structural` change
    /// that earns a majority still routes to [`Outcome::Propose`] (human
    /// approval) rather than [`Outcome::Land`].
    pub invasiveness: Invasiveness,
}

/// Invasiveness ceiling of a proposed change.
///
/// Mirrors `engram_agents::invasiveness::Invasiveness` and
/// `engram_core::config::InvasivenessLevel` (kept as an independent copy for the
/// same no-upward-dependency reason as [`ProposedChange`]). Only the ordering
/// that matters to convergence is encoded: `Structural` is the one level that
/// forces the PROPOSE path even on a passing vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invasiveness {
    /// Pure cosmetic maintenance.
    Mechanical,
    /// Additive-only safe changes.
    Additive,
    /// Modifies existing content.
    Editorial,
    /// File creation/deletion or identity-critical frontmatter — always routes
    /// to PROPOSE (human approval) when it would otherwise LAND.
    Structural,
}

impl Invasiveness {
    /// Whether a change at this level must route to [`Outcome::Propose`] rather
    /// than [`Outcome::Land`] even when the vote passes. Only `Structural`
    /// changes require explicit human approval per
    /// `01-agents-and-council.md` §Invasiveness ceilings.
    pub fn requires_human_approval(self) -> bool {
        matches!(self, Invasiveness::Structural)
    }
}
