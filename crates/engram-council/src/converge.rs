//! Convergence: tally a round's votes into a terminal outcome.
//!
//! Per `01-agents-and-council.md` §State machine → CONVERGE, the three outcomes
//! are:
//!
//! - **LAND** — all approve, or a majority approve with no reject. Writes the
//!   change to the working tree, unstaged.
//! - **PROPOSE** — would land, but the change is high-invasiveness
//!   (`Structural`); enters the explicit human-approval queue instead.
//! - **SHELVE** — any reject, or no majority. Stored with dissent annotated.

use crate::vote::{Vote, VoteKind};
use crate::ProposedChange;

/// The terminal outcome of a council deliberation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Convergent approval at a ceiling that permits autonomous (unstaged)
    /// write. The change is written to the working tree; the human reviews the
    /// diff and stages/commits or restores. No agent ever runs `git add`.
    Land,
    /// Convergent approval, but the change requires explicit human approval
    /// before being written (either because it is `Structural`, or by policy).
    /// Carries the reason so the transcript and review queue can explain it.
    Propose { reason: String },
    /// No convergence: at least one reject, or no majority approval. Carries a
    /// human-readable reason; dissent is preserved in the transcript.
    Shelve { reason: String },
}

/// Tally the votes of a (final) CRITIQUE round into an [`Outcome`].
///
/// Only votes that [`Vote::counts`] (i.e. passed the Steelman gate) are
/// considered — a critical critique that failed the gate neither shelves nor
/// blocks the proposal.
///
/// Decision order (first match wins):
///
/// 1. **Any counted reject → SHELVE.** A single defensible reject is
///    disqualifying.
/// 2. **No counted votes at all → SHELVE.** An empty council (or every vote
///    gated out) cannot converge; nothing legitimises an autonomous write.
/// 3. **Majority approve (> half of counted votes) and no reject → LAND**,
///    downgraded to **PROPOSE** when the change
///    [`requires_human_approval`](crate::Invasiveness::requires_human_approval).
/// 4. **Otherwise (no majority) → SHELVE.** Includes the all-`RequestChanges`
///    and the tied cases — these should have driven a REVISE round upstream;
///    reaching convergence without a majority is a non-result.
pub fn tally(change: &ProposedChange, votes: &[Vote]) -> Outcome {
    let counted: Vec<&Vote> = votes.iter().filter(|v| v.counts()).collect();

    // 1. Any reject is disqualifying.
    if let Some(rejecter) = counted.iter().find(|v| v.kind == VoteKind::Reject) {
        return Outcome::Shelve {
            reason: format!("rejected by {}: {}", rejecter.agent, rejecter.rationale),
        };
    }

    // 2. No votes count — cannot converge.
    if counted.is_empty() {
        return Outcome::Shelve {
            reason: "no counted votes — council could not converge".to_string(),
        };
    }

    let approvals = counted
        .iter()
        .filter(|v| v.kind == VoteKind::Approve)
        .count();

    // 3. Majority approve, no reject (rejects already returned above).
    //    Majority = strictly more than half of the counted votes.
    if approvals * 2 > counted.len() {
        if change.invasiveness.requires_human_approval() {
            return Outcome::Propose {
                reason: format!(
                    "{} approved but change is structural — routing to human approval",
                    approvals
                ),
            };
        }
        return Outcome::Land;
    }

    // 4. No majority (and no reject): the round produced only RequestChanges or
    //    a sub-majority of approvals. Not a convergent result.
    Outcome::Shelve {
        reason: format!(
            "no majority: {} of {} counted votes approved",
            approvals,
            counted.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Invasiveness;

    fn change(inv: Invasiveness) -> ProposedChange {
        ProposedChange {
            proposing_agent: "synthesizer".into(),
            rationale: "name the concept three notes circle".into(),
            affected_paths: vec!["concepts/attention.md".into()],
            invasiveness: inv,
        }
    }

    fn approve(agent: &str) -> Vote {
        Vote::new(agent, VoteKind::Approve, "looks good")
    }
    fn reject(agent: &str) -> Vote {
        Vote::new(agent, VoteKind::Reject, "duplicates an existing note")
    }
    fn request(agent: &str) -> Vote {
        Vote::new(agent, VoteKind::RequestChanges, "tighten the claim")
    }

    #[test]
    fn unanimous_approve_additive_lands() {
        let votes = vec![approve("a"), approve("b"), approve("c")];
        assert_eq!(
            tally(&change(Invasiveness::Additive), &votes),
            Outcome::Land
        );
    }

    #[test]
    fn majority_approve_no_reject_lands() {
        // 2 of 3 approve, 1 requests changes, none reject → majority → LAND.
        let votes = vec![approve("a"), approve("b"), request("c")];
        assert_eq!(
            tally(&change(Invasiveness::Editorial), &votes),
            Outcome::Land
        );
    }

    #[test]
    fn any_reject_shelves_even_with_majority_approve() {
        // 3 approve, 1 reject → the reject is disqualifying.
        let votes = vec![approve("a"), approve("b"), approve("c"), reject("d")];
        match tally(&change(Invasiveness::Additive), &votes) {
            Outcome::Shelve { reason } => assert!(reason.contains("rejected by d")),
            other => panic!("expected Shelve, got {other:?}"),
        }
    }

    #[test]
    fn structural_majority_proposes_not_lands() {
        let votes = vec![approve("a"), approve("b"), approve("c")];
        match tally(&change(Invasiveness::Structural), &votes) {
            Outcome::Propose { reason } => assert!(reason.contains("structural")),
            other => panic!("expected Propose, got {other:?}"),
        }
    }

    #[test]
    fn tie_is_no_majority_and_shelves() {
        // 1 approve, 1 request — approvals (1) * 2 == counted (2), not > → no majority.
        let votes = vec![approve("a"), request("b")];
        match tally(&change(Invasiveness::Additive), &votes) {
            Outcome::Shelve { reason } => assert!(reason.contains("no majority")),
            other => panic!("expected Shelve, got {other:?}"),
        }
    }

    #[test]
    fn all_request_changes_shelves() {
        let votes = vec![request("a"), request("b")];
        match tally(&change(Invasiveness::Additive), &votes) {
            Outcome::Shelve { reason } => assert!(reason.contains("no majority")),
            other => panic!("expected Shelve, got {other:?}"),
        }
    }

    #[test]
    fn empty_council_shelves() {
        match tally(&change(Invasiveness::Additive), &[]) {
            Outcome::Shelve { reason } => assert!(reason.contains("no counted votes")),
            other => panic!("expected Shelve, got {other:?}"),
        }
    }

    #[test]
    fn gate_failed_reject_is_ignored() {
        // A critical reject that failed the Steelman gate must NOT shelve a
        // proposal the rest of the council approved.
        let mut bad_reject = reject("devils-advocate");
        bad_reject.gated = false; // failed the gate
        let votes = vec![approve("a"), approve("b"), bad_reject];
        assert_eq!(
            tally(&change(Invasiveness::Additive), &votes),
            Outcome::Land
        );
    }

    #[test]
    fn gate_failed_votes_do_not_count_toward_quorum_size() {
        // 1 real approve + 1 gated-out reject → only the approve counts →
        // 1 of 1 is a majority → LAND.
        let mut gated_out = reject("heretic");
        gated_out.gated = false;
        let votes = vec![approve("a"), gated_out];
        assert_eq!(
            tally(&change(Invasiveness::Additive), &votes),
            Outcome::Land
        );
    }

    #[test]
    fn solo_approver_lands() {
        let votes = vec![approve("synthesizer")];
        assert_eq!(
            tally(&change(Invasiveness::Additive), &votes),
            Outcome::Land
        );
    }
}
