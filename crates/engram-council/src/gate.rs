//! The Steelman rationality gate.
//!
//! Per [ADR 0007](../../../docs/design/adrs/0007-steelman-rationality-gate.md)
//! and `01-agents-and-council.md` §The rationality gate, every critical agent's
//! critique (Devil's Advocate, Heretic, Socratic Prober) must clear a
//! **five-criterion test** before it can count in a council vote. The criteria,
//! all of which must hold:
//!
//! 1. **Engages the actual claim** — not a strawman simplification.
//! 2. **Uses real evidence** — vault citation or verifiable external source.
//! 3. **Internally consistent** — a coherent alternative, not mere negation.
//! 4. **Has real-world adherents** — a thinker the user respects could hold it.
//! 5. **Concedes what's true** — acknowledges what the original got right.
//!
//! If all five hold the critique **passes**. Otherwise the critic gets exactly
//! one revision attempt: the first failure returns
//! [`GateVerdict::RequestRevision`] naming the failed criteria; a second failure
//! (post-revision) [`GateVerdict::Shelve`]s the critique with the explicit "no
//! defensible critique found" signal — which is itself useful information (the
//! note is robust at this level).
//!
//! # Scope of this slice (#35)
//!
//! This module is the **pure decision core** of the gate, in the same spirit as
//! the rest of `engram-council` ([`crate::converge`], [`crate::state`]): it is
//! synchronous and side-effect-free. It owns:
//!
//! - the [`Criterion`] taxonomy and the [`FiveCriteria`] structured assessment;
//! - [`SteelmanGate::evaluate`], which maps an assessment + the
//!   [`Attempt`] (initial vs. post-revision) to a [`GateVerdict`], encoding the
//!   one-revision rule;
//! - [`GateStats`], the per-agent metrics accumulator (pass rate / shelve rate).
//!
//! # Deferred to #317 (council LLM rounds + gate wiring)
//!
//! The gate's *input* — the [`FiveCriteria`] booleans — is produced by the
//! Steelman agent's LLM call (`agents/steelman/prompt.md`), driven by the async
//! orchestration layer. Wiring `evaluate` into the CRITIQUE phase so a critical
//! agent's [`Vote::gated`](crate::Vote::gated) reflects the verdict is the
//! driver's job, tracked on #317. The seam already exists: [`crate::tally`]
//! ignores votes where `gated == false`.

/// One of the five rationality criteria from ADR 0007.
///
/// The order matches the ADR's numbered list and is the order
/// [`Criterion::ALL`] iterates — [`GateVerdict::RequestRevision`] reports failed
/// criteria in this order so the critic reads them as the ADR presents them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Criterion {
    /// Addresses what the original note actually says, not a strawman.
    EngagesActualClaim,
    /// Cites vault content or a verifiable external source, not bare assertion.
    UsesRealEvidence,
    /// The counter-position is a coherent alternative, not just negation.
    InternallyConsistent,
    /// A thinker the user would respect could plausibly hold this view.
    HasRealWorldAdherents,
    /// Acknowledges what the original got right before challenging it.
    ConcedesWhatsTrue,
}

impl Criterion {
    /// All five criteria, in ADR 0007 order. Iterating this is the single
    /// source of truth for "what does the gate check" — [`FiveCriteria`]
    /// helpers walk it so adding a criterion is one enum variant + one
    /// [`FiveCriteria`] field, and every consumer updates in lockstep.
    pub const ALL: [Criterion; 5] = [
        Criterion::EngagesActualClaim,
        Criterion::UsesRealEvidence,
        Criterion::InternallyConsistent,
        Criterion::HasRealWorldAdherents,
        Criterion::ConcedesWhatsTrue,
    ];

    /// A short human-readable label for transcripts and the revision message
    /// handed back to the critic. Phrased as the ADR phrases the criterion.
    pub fn label(self) -> &'static str {
        match self {
            Criterion::EngagesActualClaim => "engages the actual claim (not a strawman)",
            Criterion::UsesRealEvidence => "uses real evidence (citation, not assertion)",
            Criterion::InternallyConsistent => "internally consistent (coherent alternative)",
            Criterion::HasRealWorldAdherents => "has real-world adherents",
            Criterion::ConcedesWhatsTrue => "concedes what the original got right",
        }
    }
}

/// The structured per-criterion judgment for a single critique.
///
/// Each field is the Steelman agent's boolean verdict on one criterion. The
/// agent's prompt (`agents/steelman/prompt.md`) emits these plus per-criterion
/// rationale; this pure core consumes only the booleans — the rationale travels
/// with the agent's typed output
/// (`engram_agents::agents::steelman::SteelmanOutput`) into the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FiveCriteria {
    /// Criterion 1 — see [`Criterion::EngagesActualClaim`].
    pub engages_actual_claim: bool,
    /// Criterion 2 — see [`Criterion::UsesRealEvidence`].
    pub uses_real_evidence: bool,
    /// Criterion 3 — see [`Criterion::InternallyConsistent`].
    pub internally_consistent: bool,
    /// Criterion 4 — see [`Criterion::HasRealWorldAdherents`].
    pub has_real_world_adherents: bool,
    /// Criterion 5 — see [`Criterion::ConcedesWhatsTrue`].
    pub concedes_whats_true: bool,
}

impl FiveCriteria {
    /// All five criteria holding — the only assessment that [`SteelmanGate`]
    /// passes regardless of attempt.
    pub const ALL_PASS: FiveCriteria = FiveCriteria {
        engages_actual_claim: true,
        uses_real_evidence: true,
        internally_consistent: true,
        has_real_world_adherents: true,
        concedes_whats_true: true,
    };

    /// Whether the given criterion holds in this assessment.
    pub fn holds(&self, criterion: Criterion) -> bool {
        match criterion {
            Criterion::EngagesActualClaim => self.engages_actual_claim,
            Criterion::UsesRealEvidence => self.uses_real_evidence,
            Criterion::InternallyConsistent => self.internally_consistent,
            Criterion::HasRealWorldAdherents => self.has_real_world_adherents,
            Criterion::ConcedesWhatsTrue => self.concedes_whats_true,
        }
    }

    /// `true` iff every criterion holds (the critique is defensible).
    pub fn all_pass(&self) -> bool {
        Criterion::ALL.iter().all(|&c| self.holds(c))
    }

    /// The criteria that failed, in [`Criterion::ALL`] order. Empty iff
    /// [`all_pass`](Self::all_pass).
    pub fn failed(&self) -> Vec<Criterion> {
        Criterion::ALL
            .iter()
            .copied()
            .filter(|&c| !self.holds(c))
            .collect()
    }
}

/// Which round of the gate a critique is in.
///
/// The gate allows exactly one revision (ADR 0007: "Maximum one revision
/// round"). The driver passes [`Attempt::Initial`] for a critique's first pass;
/// if the verdict is [`GateVerdict::RequestRevision`], the critic revises and
/// the driver re-evaluates the revised critique with [`Attempt::PostRevision`].
/// A second failure shelves.
///
/// Keeping the attempt as a caller-supplied value (rather than a counter inside
/// [`SteelmanGate`]) mirrors [`crate::state::CouncilSession`], which owns round
/// state and feeds the pure core each round — the gate stays side-effect-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// The critique's first pass through the gate.
    Initial,
    /// The critique after one revision. A failure here is terminal (shelve).
    PostRevision,
}

/// The gate's verdict on a single critique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// All five criteria held. The critique counts — the driver sets the
    /// corresponding [`Vote::gated`](crate::Vote::gated) to `true`.
    Pass,
    /// One or more criteria failed on the [`Attempt::Initial`] pass. The critic
    /// gets one revision attempt with the failed criteria named.
    RequestRevision {
        /// The criteria that failed, in [`Criterion::ALL`] order.
        failed_criteria: Vec<Criterion>,
        /// A ready-to-surface message naming the failed criteria, for the
        /// transcript and the critic's revision prompt.
        explanation: String,
    },
    /// The critique failed after its one revision (or failed terminally). It is
    /// shelved with the explicit "no defensible critique found" signal. The
    /// driver sets [`Vote::gated`](crate::Vote::gated) to `false`, so
    /// [`crate::tally`] ignores it.
    Shelve {
        /// Human-readable reason, recorded in the deliberation transcript.
        reason: String,
    },
}

impl GateVerdict {
    /// Whether this verdict lets the critique count toward the council tally.
    /// Only [`GateVerdict::Pass`] does.
    pub fn passed(&self) -> bool {
        matches!(self, GateVerdict::Pass)
    }
}

/// The Steelman rationality gate (ADR 0007).
///
/// A zero-sized dispatcher over the pure decision logic — there is no per-gate
/// state to hold (the one-revision rule is carried by the [`Attempt`] the
/// caller supplies, exactly as [`crate::state::CouncilSession`] carries round
/// state). Aggregate metrics live in [`GateStats`], which the caller owns and
/// keys per critical agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SteelmanGate;

impl SteelmanGate {
    /// Evaluate a critique's [`FiveCriteria`] assessment against the gate.
    ///
    /// - **All five hold** → [`GateVerdict::Pass`], regardless of attempt.
    /// - **Any fail on [`Attempt::Initial`]** → [`GateVerdict::RequestRevision`]
    ///   naming the failed criteria.
    /// - **Any fail on [`Attempt::PostRevision`]** → [`GateVerdict::Shelve`]
    ///   with "no defensible critique found".
    ///
    /// The gate is structural: it depends only on the criteria assessment, not
    /// on trust score or invasiveness ceiling (ADR 0007).
    pub fn evaluate(criteria: &FiveCriteria, attempt: Attempt) -> GateVerdict {
        if criteria.all_pass() {
            return GateVerdict::Pass;
        }
        let failed = criteria.failed();
        match attempt {
            Attempt::Initial => GateVerdict::RequestRevision {
                explanation: revision_message(&failed),
                failed_criteria: failed,
            },
            Attempt::PostRevision => GateVerdict::Shelve {
                reason: shelve_message(&failed),
            },
        }
    }
}

/// Build the revision message naming the failed criteria for the critic.
fn revision_message(failed: &[Criterion]) -> String {
    format!(
        "Critique returned for revision — it fails {} of the five rationality criteria: {}. \
         Revise to address {} (one revision allowed).",
        failed.len(),
        join_labels(failed),
        if failed.len() == 1 { "it" } else { "them" },
    )
}

/// Build the shelve reason after a critique fails its one revision.
fn shelve_message(failed: &[Criterion]) -> String {
    format!(
        "No defensible critique found — after one revision it still fails: {}. \
         Shelved; the original is robust to attack at this level.",
        join_labels(failed),
    )
}

/// Comma-join criterion labels for a message (`"a; b; c"`).
fn join_labels(failed: &[Criterion]) -> String {
    failed
        .iter()
        .map(|c| c.label())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Per-agent gate metrics (ADR 0007: "critique pass rate per critical agent;
/// shelved-with-no-defensible-critique rate").
///
/// The accumulator is agent-agnostic; the caller keeps one per critical agent
/// (e.g. a `HashMap<String, GateStats>` keyed by agent name) and
/// [`record`](Self::record)s each verdict. A `RequestRevision` is **not** a
/// terminal outcome — the same critique returns as a `Pass` or `Shelve` after
/// revision — so it is counted separately and excluded from the rate
/// denominators, which count only terminal verdicts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateStats {
    /// Critiques that passed the gate.
    pub passed: u32,
    /// Revision requests issued (non-terminal; the critique is re-evaluated).
    pub revisions_requested: u32,
    /// Critiques shelved as "no defensible critique found".
    pub shelved: u32,
}

impl GateStats {
    /// Fold a verdict into the running counts.
    pub fn record(&mut self, verdict: &GateVerdict) {
        match verdict {
            GateVerdict::Pass => self.passed += 1,
            GateVerdict::RequestRevision { .. } => self.revisions_requested += 1,
            GateVerdict::Shelve { .. } => self.shelved += 1,
        }
    }

    /// Terminal verdicts: passes + shelves. Revision requests are excluded —
    /// they resolve into one of these on the next evaluation.
    pub fn terminal(&self) -> u32 {
        self.passed + self.shelved
    }

    /// Fraction of terminal critiques that passed, or `None` when no terminal
    /// verdict has been recorded yet (avoids a 0/0 that reads as "0% pass").
    pub fn pass_rate(&self) -> Option<f64> {
        match self.terminal() {
            0 => None,
            t => Some(f64::from(self.passed) / f64::from(t)),
        }
    }

    /// Fraction of terminal critiques shelved as "no defensible critique
    /// found", or `None` when no terminal verdict has been recorded yet.
    pub fn shelve_rate(&self) -> Option<f64> {
        match self.terminal() {
            0 => None,
            t => Some(f64::from(self.shelved) / f64::from(t)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An assessment with exactly one criterion failing (the rest passing).
    fn one_failing(failing: Criterion) -> FiveCriteria {
        let mut c = FiveCriteria::ALL_PASS;
        match failing {
            Criterion::EngagesActualClaim => c.engages_actual_claim = false,
            Criterion::UsesRealEvidence => c.uses_real_evidence = false,
            Criterion::InternallyConsistent => c.internally_consistent = false,
            Criterion::HasRealWorldAdherents => c.has_real_world_adherents = false,
            Criterion::ConcedesWhatsTrue => c.concedes_whats_true = false,
        }
        c
    }

    // --- Per-criterion positive + negative coverage (ADR 0007 requires each
    //     of the 5 to have at least one positive and one negative example). ---

    #[test]
    fn each_criterion_holds_in_all_pass() {
        // Positive example for every criterion: all hold in ALL_PASS.
        for c in Criterion::ALL {
            assert!(
                FiveCriteria::ALL_PASS.holds(c),
                "{c:?} should hold in ALL_PASS"
            );
        }
        assert!(FiveCriteria::ALL_PASS.all_pass());
        assert!(FiveCriteria::ALL_PASS.failed().is_empty());
    }

    #[test]
    fn each_criterion_can_fail_independently() {
        // Negative example for every criterion: flipping exactly one to false
        // makes it (and only it) the failure, and breaks all_pass.
        for c in Criterion::ALL {
            let assessment = one_failing(c);
            assert!(!assessment.holds(c), "{c:?} should fail when flipped off");
            assert!(!assessment.all_pass(), "{c:?} off must break all_pass");
            assert_eq!(
                assessment.failed(),
                vec![c],
                "{c:?} should be the sole failure"
            );
        }
    }

    #[test]
    fn failed_preserves_adr_order() {
        // Two non-adjacent criteria fail; failed() reports them in ALL order.
        let mut c = FiveCriteria::ALL_PASS;
        c.concedes_whats_true = false; // criterion 5
        c.engages_actual_claim = false; // criterion 1
        assert_eq!(
            c.failed(),
            vec![Criterion::EngagesActualClaim, Criterion::ConcedesWhatsTrue]
        );
    }

    // --- Verdict classification ---

    #[test]
    fn all_pass_passes_on_either_attempt() {
        assert_eq!(
            SteelmanGate::evaluate(&FiveCriteria::ALL_PASS, Attempt::Initial),
            GateVerdict::Pass
        );
        assert_eq!(
            SteelmanGate::evaluate(&FiveCriteria::ALL_PASS, Attempt::PostRevision),
            GateVerdict::Pass
        );
    }

    #[test]
    fn initial_failure_requests_revision_naming_criteria() {
        let assessment = one_failing(Criterion::EngagesActualClaim);
        match SteelmanGate::evaluate(&assessment, Attempt::Initial) {
            GateVerdict::RequestRevision {
                failed_criteria,
                explanation,
            } => {
                assert_eq!(failed_criteria, vec![Criterion::EngagesActualClaim]);
                assert!(explanation.contains("engages the actual claim"));
                assert!(explanation.contains("one revision"));
            }
            other => panic!("expected RequestRevision, got {other:?}"),
        }
    }

    #[test]
    fn post_revision_failure_shelves_with_no_defensible_critique() {
        let assessment = one_failing(Criterion::UsesRealEvidence);
        match SteelmanGate::evaluate(&assessment, Attempt::PostRevision) {
            GateVerdict::Shelve { reason } => {
                assert!(reason.contains("No defensible critique found"));
                assert!(reason.contains("uses real evidence"));
            }
            other => panic!("expected Shelve, got {other:?}"),
        }
    }

    #[test]
    fn multiple_failures_all_named_in_revision() {
        let assessment = FiveCriteria {
            engages_actual_claim: false,
            uses_real_evidence: false,
            internally_consistent: true,
            has_real_world_adherents: true,
            concedes_whats_true: false,
        };
        match SteelmanGate::evaluate(&assessment, Attempt::Initial) {
            GateVerdict::RequestRevision {
                failed_criteria, ..
            } => assert_eq!(failed_criteria.len(), 3),
            other => panic!("expected RequestRevision, got {other:?}"),
        }
    }

    #[test]
    fn verdict_passed_helper() {
        assert!(GateVerdict::Pass.passed());
        assert!(!GateVerdict::Shelve { reason: "x".into() }.passed());
        assert!(!GateVerdict::RequestRevision {
            failed_criteria: vec![Criterion::UsesRealEvidence],
            explanation: "x".into(),
        }
        .passed());
    }

    // --- Metrics ---

    #[test]
    fn stats_count_verdicts_and_compute_rates() {
        let mut stats = GateStats::default();
        assert_eq!(stats.pass_rate(), None, "no data → no rate");

        stats.record(&GateVerdict::Pass);
        stats.record(&GateVerdict::Pass);
        stats.record(&GateVerdict::Pass);
        stats.record(&GateVerdict::Shelve { reason: "x".into() });
        stats.record(&GateVerdict::RequestRevision {
            failed_criteria: vec![Criterion::UsesRealEvidence],
            explanation: "x".into(),
        });

        assert_eq!(stats.passed, 3);
        assert_eq!(stats.shelved, 1);
        assert_eq!(stats.revisions_requested, 1);
        // Revision requests are non-terminal: 4 terminal (3 pass + 1 shelve).
        assert_eq!(stats.terminal(), 4);
        assert_eq!(stats.pass_rate(), Some(0.75));
        assert_eq!(stats.shelve_rate(), Some(0.25));
    }
}
