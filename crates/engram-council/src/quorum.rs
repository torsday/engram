//! Quorum selection: who participates in a given council.
//!
//! Per `01-agents-and-council.md` §Who participates: the council is the
//! convening agent + every agent whose `participates = true` in config + any
//! agents explicitly relevant to the change (e.g. Cartographer when MOCs are
//! affected). **Not** the full roster every time.

/// Inputs to quorum selection. All agent names are kebab-case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumInput {
    /// The agent convening the council. Always in the quorum.
    pub convening_agent: String,
    /// Agents whose config sets `participates = true` (the standing council
    /// members), in roster order.
    pub opt_in_participants: Vec<String>,
    /// Agents pulled in because they are explicitly relevant to *this* change
    /// (e.g. `cartographer` when a MOC path is affected). The async driver
    /// computes relevance; the core just unions them in.
    pub relevant_agents: Vec<String>,
}

/// Select the quorum for a council.
///
/// Returns the convening agent first, then opt-in participants, then
/// change-relevant agents — deduplicated, preserving first-seen order so the
/// result is deterministic (important for reproducible transcripts and tests).
/// The convening agent is never duplicated even if it also appears in the
/// opt-in or relevant lists.
pub fn select_quorum(input: &QuorumInput) -> Vec<String> {
    let mut quorum = Vec::new();
    let push_unique = |name: &str, q: &mut Vec<String>| {
        if !q.iter().any(|existing| existing == name) {
            q.push(name.to_string());
        }
    };

    push_unique(&input.convening_agent, &mut quorum);
    for a in &input.opt_in_participants {
        push_unique(a, &mut quorum);
    }
    for a in &input.relevant_agents {
        push_unique(a, &mut quorum);
    }
    quorum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> QuorumInput {
        QuorumInput {
            convening_agent: "synthesizer".into(),
            opt_in_participants: vec!["devils-advocate".into(), "voice-keeper".into()],
            relevant_agents: vec!["cartographer".into()],
        }
    }

    #[test]
    fn convening_agent_is_first() {
        let q = select_quorum(&input());
        assert_eq!(q[0], "synthesizer");
    }

    #[test]
    fn unions_all_three_sources_in_order() {
        let q = select_quorum(&input());
        assert_eq!(
            q,
            vec![
                "synthesizer".to_string(),
                "devils-advocate".to_string(),
                "voice-keeper".to_string(),
                "cartographer".to_string(),
            ]
        );
    }

    #[test]
    fn deduplicates_convening_agent_appearing_elsewhere() {
        let mut i = input();
        // Synthesizer also opted in and is "relevant" — must appear once.
        i.opt_in_participants.push("synthesizer".into());
        i.relevant_agents.push("synthesizer".into());
        let q = select_quorum(&i);
        assert_eq!(q.iter().filter(|a| *a == "synthesizer").count(), 1);
        assert_eq!(q[0], "synthesizer");
    }

    #[test]
    fn deduplicates_across_optin_and_relevant() {
        let mut i = input();
        // Cartographer both opted in and is change-relevant.
        i.opt_in_participants.push("cartographer".into());
        let q = select_quorum(&i);
        assert_eq!(q.iter().filter(|a| *a == "cartographer").count(), 1);
    }

    #[test]
    fn solo_council_is_just_the_convener() {
        let q = select_quorum(&QuorumInput {
            convening_agent: "linker".into(),
            opt_in_participants: vec![],
            relevant_agents: vec![],
        });
        assert_eq!(q, vec!["linker".to_string()]);
    }
}
