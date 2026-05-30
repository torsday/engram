//! Votes cast by participating agents during a CRITIQUE round.

/// How an agent voted on the proposal under review.
///
/// Per `01-agents-and-council.md` §State machine → CRITIQUE, each participant
/// returns one of three vote kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteKind {
    /// The proposal is good as-is.
    Approve,
    /// The proposal needs changes before it can land; the proposer gets one
    /// chance to revise (drives the REVISE phase).
    RequestChanges,
    /// The proposal should not land. A single reject forces
    /// [`crate::Outcome::Shelve`] at convergence.
    Reject,
}

/// One agent's vote in a CRITIQUE round.
///
/// `suggested_edits` is intentionally a plain `Option<String>` (a unified-diff
/// or replacement-body blob) rather than a structured diff type — the council
/// core never *applies* edits; it only tallies votes and preserves the
/// proposer's revision material. The async driver and the persistence layer are
/// the consumers of the edit payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vote {
    /// Kebab-case name of the voting agent.
    pub agent: String,
    /// The vote itself.
    pub kind: VoteKind,
    /// One-paragraph rationale (soft length-capped by the prompt, not here).
    pub rationale: String,
    /// Optional suggested edit on top of the proposal.
    pub suggested_edits: Option<String>,
    /// Whether this vote passed the Steelman rationality gate (#35).
    ///
    /// `true` for non-critical agents (the gate only applies to critical roles)
    /// and for critical votes that survived the five-criterion test. `false`
    /// when a critical agent's critique failed the gate — such a vote is
    /// **ignored** by [`crate::tally`] (a gate-failed reject does not shelve a
    /// proposal). Until #35 lands, the driver sets this to `true`.
    pub gated: bool,
}

impl Vote {
    /// Construct a gate-passing vote (the common case: non-critical agents, and
    /// the pre-#35 default where every vote is treated as gated-in).
    pub fn new(agent: impl Into<String>, kind: VoteKind, rationale: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            kind,
            rationale: rationale.into(),
            suggested_edits: None,
            gated: true,
        }
    }

    /// Whether this vote counts toward the tally. A vote counts unless it was a
    /// critical critique that failed the Steelman gate (`gated == false`).
    pub fn counts(&self) -> bool {
        self.gated
    }
}
