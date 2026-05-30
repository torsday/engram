//! The council state machine.
//!
//! Per `01-agents-and-council.md` §State machine, a deliberation advances
//! through bounded phases:
//!
//! ```text
//! DRAFT → CRITIQUE(1) → [REVISE → CRITIQUE(2)]? → CONVERGE → Terminal(Outcome)
//! ```
//!
//! with **at most two CRITIQUE rounds** (the initial round, plus one post-revision
//! round). [`CouncilSession`] is the driveable handle: the async orchestration
//! layer constructs it, feeds it each round's [`Vote`]s, optionally feeds a
//! revision, and reads back the terminal [`Outcome`].

use ulid::Ulid;

use crate::converge::{tally, Outcome};
use crate::quorum::{select_quorum, QuorumInput};
use crate::vote::Vote;
use crate::ProposedChange;

/// Maximum number of CRITIQUE rounds: the initial round plus one post-revision
/// round, per the design's "Maximum 2 total rounds" bound.
pub const MAX_CRITIQUE_ROUNDS: u8 = 2;

/// Which phase the deliberation is in.
///
/// `Critique` carries the 1-based round number. `Terminal` carries the decided
/// [`Outcome`]; once terminal the session accepts no further input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// The proposing agent has submitted the change; no critique yet.
    Draft,
    /// A CRITIQUE round is open, awaiting votes. `round` is 1-based.
    Critique { round: u8 },
    /// At least one agent requested changes in the prior round; the proposer
    /// may revise before the next CRITIQUE round.
    Revise,
    /// Votes are being tallied into an outcome.
    Converge,
    /// Decided. No further transitions.
    Terminal(Outcome),
}

/// Errors from driving the state machine out of order.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CouncilError {
    /// `record_round` was called in a phase that is not awaiting votes.
    #[error("cannot record a critique round in phase {phase:?}; expected an open Critique phase")]
    NotAwaitingVotes {
        /// The phase the session was actually in.
        phase: Phase,
    },
    /// `revise` was called when the session was not in the REVISE phase.
    #[error("cannot revise in phase {phase:?}; revision is only allowed after a round that requested changes")]
    NotRevisable {
        /// The phase the session was actually in.
        phase: Phase,
    },
    /// The session is already terminal.
    #[error("council already converged to a terminal outcome")]
    AlreadyTerminal,
}

/// A live council deliberation.
///
/// Construct with [`CouncilSession::convene`], then drive it:
///
/// 1. [`record_round`](Self::record_round) with the CRITIQUE round's votes.
///    - If the votes converge (or the round budget is exhausted), the session
///      becomes [`Phase::Terminal`] and `record_round` returns the [`Outcome`].
///    - If any vote requested changes and a round remains, the session moves to
///      [`Phase::Revise`] and returns `None`.
/// 2. [`revise`](Self::revise) when in REVISE, supplying the proposer's revised
///    change; the session re-opens CRITIQUE for the next round.
///
/// The session never calls an LLM or touches disk; the driver supplies votes
/// and revisions from whatever source (real agents, a test script).
#[derive(Debug, Clone)]
pub struct CouncilSession {
    id: Ulid,
    change: ProposedChange,
    quorum: Vec<String>,
    phase: Phase,
    rounds_held: u8,
}

impl CouncilSession {
    /// Convene a council over `change` with the given quorum inputs. The
    /// session starts in [`Phase::Draft`]; call
    /// [`record_round`](Self::record_round) to run the first CRITIQUE round
    /// (DRAFT opens round 1 implicitly).
    pub fn convene(change: ProposedChange, quorum_input: &QuorumInput) -> Self {
        Self {
            id: Ulid::new(),
            change,
            quorum: select_quorum(quorum_input),
            phase: Phase::Draft,
            rounds_held: 0,
        }
    }

    /// The deliberation's unique id (used as the transcript / SQLite key by the
    /// persistence layer).
    pub fn id(&self) -> Ulid {
        self.id
    }

    /// The change under deliberation.
    pub fn change(&self) -> &ProposedChange {
        &self.change
    }

    /// The selected quorum (agent names, order-stable).
    pub fn quorum(&self) -> &[String] {
        &self.quorum
    }

    /// Current phase.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Number of CRITIQUE rounds already held.
    pub fn rounds_held(&self) -> u8 {
        self.rounds_held
    }

    /// The terminal outcome, if the session has converged.
    pub fn outcome(&self) -> Option<&Outcome> {
        match &self.phase {
            Phase::Terminal(o) => Some(o),
            _ => None,
        }
    }

    /// Record the votes for the current CRITIQUE round and advance.
    ///
    /// Returns `Ok(Some(outcome))` when the deliberation converges this round —
    /// either because the votes tally to LAND/PROPOSE/SHELVE with no open
    /// revision path, or because the round budget ([`MAX_CRITIQUE_ROUNDS`]) is
    /// exhausted. Returns `Ok(None)` when at least one vote requested changes
    /// and a round remains: the session moves to [`Phase::Revise`] awaiting a
    /// [`revise`](Self::revise) call.
    ///
    /// # Errors
    ///
    /// [`CouncilError::AlreadyTerminal`] if the session has already converged;
    /// [`CouncilError::NotAwaitingVotes`] is not returned here because DRAFT and
    /// an open CRITIQUE are both valid entry points (DRAFT opens round 1
    /// implicitly).
    pub fn record_round(&mut self, votes: &[Vote]) -> Result<Option<Outcome>, CouncilError> {
        match self.phase {
            Phase::Terminal(_) => return Err(CouncilError::AlreadyTerminal),
            Phase::Draft | Phase::Critique { .. } | Phase::Revise => {}
            Phase::Converge => {
                // Converge is a transient internal phase; being asked to record
                // votes while in it is a driver bug.
                return Err(CouncilError::NotAwaitingVotes {
                    phase: self.phase.clone(),
                });
            }
        }

        self.rounds_held += 1;
        self.phase = Phase::Converge;

        let wants_revision = votes
            .iter()
            .any(|v| v.counts() && v.kind == crate::vote::VoteKind::RequestChanges);
        let has_reject = votes
            .iter()
            .any(|v| v.counts() && v.kind == crate::vote::VoteKind::Reject);
        let rounds_remain = self.rounds_held < MAX_CRITIQUE_ROUNDS;

        // A revision round is warranted only when someone asked for changes,
        // nobody outright rejected (a reject is terminal — no point revising),
        // and we still have a round in the budget.
        if wants_revision && !has_reject && rounds_remain {
            self.phase = Phase::Revise;
            return Ok(None);
        }

        let outcome = tally(&self.change, votes);
        self.phase = Phase::Terminal(outcome.clone());
        Ok(Some(outcome))
    }

    /// Supply the proposer's revised change and re-open CRITIQUE for the next
    /// round. Only valid in [`Phase::Revise`].
    ///
    /// # Errors
    ///
    /// [`CouncilError::NotRevisable`] if the session is not in REVISE.
    pub fn revise(&mut self, revised: ProposedChange) -> Result<(), CouncilError> {
        if self.phase != Phase::Revise {
            return Err(CouncilError::NotRevisable {
                phase: self.phase.clone(),
            });
        }
        self.change = revised;
        self.phase = Phase::Critique {
            round: self.rounds_held + 1,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vote::{Vote, VoteKind};
    use crate::Invasiveness;

    fn change() -> ProposedChange {
        ProposedChange {
            proposing_agent: "synthesizer".into(),
            rationale: "name the concept".into(),
            affected_paths: vec!["concepts/x.md".into()],
            invasiveness: Invasiveness::Additive,
        }
    }

    fn quorum_input() -> QuorumInput {
        QuorumInput {
            convening_agent: "synthesizer".into(),
            opt_in_participants: vec!["devils-advocate".into(), "linker".into()],
            relevant_agents: vec![],
        }
    }

    fn approve(a: &str) -> Vote {
        Vote::new(a, VoteKind::Approve, "ok")
    }
    fn request(a: &str) -> Vote {
        Vote::new(a, VoteKind::RequestChanges, "tighten")
    }
    fn reject(a: &str) -> Vote {
        Vote::new(a, VoteKind::Reject, "dup")
    }

    #[test]
    fn convene_starts_in_draft_with_quorum() {
        let s = CouncilSession::convene(change(), &quorum_input());
        assert_eq!(*s.phase(), Phase::Draft);
        assert_eq!(s.quorum(), &["synthesizer", "devils-advocate", "linker"]);
        assert_eq!(s.rounds_held(), 0);
        assert!(s.outcome().is_none());
    }

    #[test]
    fn unanimous_first_round_lands_immediately() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        let out = s
            .record_round(&[
                approve("synthesizer"),
                approve("devils-advocate"),
                approve("linker"),
            ])
            .expect("record");
        assert_eq!(out, Some(Outcome::Land));
        assert_eq!(*s.phase(), Phase::Terminal(Outcome::Land));
        assert_eq!(s.rounds_held(), 1);
    }

    #[test]
    fn request_changes_first_round_enters_revise() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        let out = s
            .record_round(&[approve("synthesizer"), request("devils-advocate")])
            .expect("record");
        assert_eq!(out, None);
        assert_eq!(*s.phase(), Phase::Revise);
        assert_eq!(s.rounds_held(), 1);
    }

    #[test]
    fn revise_then_second_round_converges() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        s.record_round(&[approve("synthesizer"), request("devils-advocate")])
            .unwrap();
        s.revise(change()).expect("revise");
        assert_eq!(*s.phase(), Phase::Critique { round: 2 });
        let out = s
            .record_round(&[approve("synthesizer"), approve("devils-advocate")])
            .expect("record round 2");
        assert_eq!(out, Some(Outcome::Land));
        assert_eq!(s.rounds_held(), 2);
    }

    #[test]
    fn round_budget_caps_at_two_even_with_more_requests() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        // Round 1: request changes → revise.
        s.record_round(&[approve("a"), request("b")]).unwrap();
        s.revise(change()).unwrap();
        // Round 2: request changes again, but budget is exhausted → must
        // converge now (no third round). No majority approve → SHELVE.
        let out = s.record_round(&[approve("a"), request("b")]).unwrap();
        assert!(matches!(out, Some(Outcome::Shelve { .. })));
        assert!(matches!(s.phase(), Phase::Terminal(_)));
        assert_eq!(s.rounds_held(), 2);
    }

    #[test]
    fn reject_first_round_shelves_without_revise() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        // A reject is terminal even though a round remains — no point revising.
        let out = s.record_round(&[approve("a"), reject("b")]).unwrap();
        assert!(matches!(out, Some(Outcome::Shelve { .. })));
        assert!(matches!(s.phase(), Phase::Terminal(_)));
    }

    #[test]
    fn revise_outside_revise_phase_errors() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        let err = s.revise(change()).expect_err("must error in Draft");
        assert!(matches!(err, CouncilError::NotRevisable { .. }));
    }

    #[test]
    fn recording_after_terminal_errors() {
        let mut s = CouncilSession::convene(change(), &quorum_input());
        s.record_round(&[approve("a")]).unwrap();
        let err = s.record_round(&[approve("a")]).expect_err("terminal");
        assert_eq!(err, CouncilError::AlreadyTerminal);
    }

    #[test]
    fn ids_are_unique_per_session() {
        let a = CouncilSession::convene(change(), &quorum_input());
        let b = CouncilSession::convene(change(), &quorum_input());
        assert_ne!(a.id(), b.id());
    }
}
