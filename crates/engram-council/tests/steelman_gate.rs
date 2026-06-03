//! Integration test for the Steelman rationality gate (#35, ADR 0007).
//!
//! Exercises the full gate lifecycle the way the council driver (#317)
//! will drive it, using *mocked* critique assessments (constructed
//! [`FiveCriteria`] rather than LLM output — the live wiring is #317).
//! The scenario from #35's acceptance criteria:
//!
//! 1. a strawman critique (fails criterion 1) gets `RequestRevision`;
//! 2. a critique that survives revision `Pass`es;
//! 3. a critique that fails its revision `Shelve`s;
//!
//! plus the end-to-end consequence that matters to the council: a
//! gate-failed critical reject must NOT shelve a proposal the rest of
//! the council approved (the `gated == false` path in [`tally`]).

use engram_council::gate::{Attempt, Criterion, FiveCriteria, GateVerdict, SteelmanGate};
use engram_council::{tally, Invasiveness, Outcome, ProposedChange, Vote, VoteKind};

/// A strawman critique: it fails criterion 1 (engages the actual
/// claim) but is otherwise fine.
fn strawman() -> FiveCriteria {
    FiveCriteria {
        engages_actual_claim: false,
        ..FiveCriteria::ALL_PASS
    }
}

#[test]
fn strawman_then_successful_revision_passes() {
    // 1. First pass: the strawman fails → one revision requested,
    //    naming criterion 1.
    let first = SteelmanGate::evaluate(&strawman(), Attempt::Initial);
    match first {
        GateVerdict::RequestRevision {
            failed_criteria, ..
        } => assert_eq!(failed_criteria, vec![Criterion::EngagesActualClaim]),
        other => panic!("expected RequestRevision on the strawman, got {other:?}"),
    }

    // 2. The critic revises to engage the real claim; now all five
    //    hold → the post-revision pass succeeds.
    let revised = FiveCriteria::ALL_PASS;
    assert_eq!(
        SteelmanGate::evaluate(&revised, Attempt::PostRevision),
        GateVerdict::Pass,
    );
}

#[test]
fn strawman_then_failed_revision_shelves() {
    // First pass fails → revision requested.
    assert!(matches!(
        SteelmanGate::evaluate(&strawman(), Attempt::Initial),
        GateVerdict::RequestRevision { .. }
    ));

    // The revision still fails (the critic couldn't engage the real
    // claim) → shelved as "no defensible critique found".
    match SteelmanGate::evaluate(&strawman(), Attempt::PostRevision) {
        GateVerdict::Shelve { reason } => {
            assert!(reason.contains("No defensible critique found"));
        }
        other => panic!("expected Shelve after a failed revision, got {other:?}"),
    }
}

#[test]
fn gate_failed_critique_does_not_shelve_an_approved_proposal() {
    // The end-to-end consequence: a critical agent's reject that the
    // gate shelved must not drag down a proposal the council approved.
    // The driver translates a Shelve verdict into `gated = false`;
    // tally then ignores that vote.
    let verdict = SteelmanGate::evaluate(&strawman(), Attempt::PostRevision);
    assert!(!verdict.passed());

    let change = ProposedChange {
        proposing_agent: "synthesizer".into(),
        rationale: "name the concept three notes circle".into(),
        affected_paths: vec!["concepts/attention.md".into()],
        invasiveness: Invasiveness::Additive,
    };

    let mut gated_out_reject = Vote::new(
        "devils-advocate",
        VoteKind::Reject,
        "the original is worthless",
    );
    // The driver sets this from the gate verdict (Shelve → not passed).
    gated_out_reject.gated = verdict.passed();

    let votes = vec![
        Vote::new("synthesizer", VoteKind::Approve, "good"),
        Vote::new("linker", VoteKind::Approve, "good"),
        gated_out_reject,
    ];

    // Two real approvals, one gate-failed reject (ignored) → LAND.
    assert_eq!(tally(&change, &votes), Outcome::Land);
}

#[test]
fn a_passing_critique_counts_as_a_real_vote() {
    // The dual: a critique that passes the gate keeps its vote, so a
    // defensible reject still shelves the proposal.
    let verdict = SteelmanGate::evaluate(&FiveCriteria::ALL_PASS, Attempt::Initial);
    assert!(verdict.passed());

    let change = ProposedChange {
        proposing_agent: "synthesizer".into(),
        rationale: "name the concept".into(),
        affected_paths: vec!["concepts/x.md".into()],
        invasiveness: Invasiveness::Additive,
    };

    let mut defensible_reject = Vote::new("devils-advocate", VoteKind::Reject, "duplicates 01H8QZ");
    defensible_reject.gated = verdict.passed(); // true — the gate passed it

    let votes = vec![
        Vote::new("synthesizer", VoteKind::Approve, "good"),
        defensible_reject,
    ];

    match tally(&change, &votes) {
        Outcome::Shelve { reason } => assert!(reason.contains("rejected by devils-advocate")),
        other => panic!("a passing reject must shelve; got {other:?}"),
    }
}
