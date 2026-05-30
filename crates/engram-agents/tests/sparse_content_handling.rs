//! Cross-agent sparse-content handling (#137).
//!
//! `docs/design/08-first-run.md` §Sparse-content bootstrap requires the
//! context-dependent agents to fail gracefully — return a structured
//! "insufficient" stance rather than fabricate — until the vault holds enough
//! human-authored material. This test pins the deterministic gates that
//! enforce that, at the canonical design-doc thresholds, for every affected
//! agent in one place:
//!
//! | Agent         | Gate                              | Thin → | Rich → |
//! |---------------|-----------------------------------|--------|--------|
//! | Biographer    | `SparseContentGate` (200 / 60d)   | abstain| run    |
//! | Annual Review | `MaturityGate` (365d)             | abstain| run    |
//! | Voice Keeper  | `VoiceKeeperBootstrap` (50/200/30d)| observe-only | mature |
//! | Witness       | none — works from day one         | n/a    | n/a    |
//! | Tutor         | none — any evergreen yields cards | n/a    | n/a    |
//! | Predictor     | per-topic calibration gate (own slice) | — | — |
//!
//! These are *pure* checks the runtime evaluates before spending LLM tokens.
//! Keeping the thin-vs-rich assertions for all gated agents in one file makes
//! a threshold regression in any single agent visible against its siblings.
//!
//! Note: #137's original acceptance criteria listed older per-agent numbers
//! (e.g. Biographer "≥ 30 notes / 5K words"); those predated 08-first-run.md.
//! The thresholds asserted here match the design doc and the gates already
//! shipped with Biographer (#57) and Annual Review (#61).

use engram_agents::agents::annual_review::MaturityGate;
use engram_agents::agents::biographer::{SparseContentGate, VaultSnapshot};
use engram_agents::agents::voice_keeper::{VoiceKeeperBootstrap, VoiceKeeperTier};

fn snap(human_notes_total: u32, age_days: u32) -> VaultSnapshot {
    VaultSnapshot {
        human_notes_total,
        age_days,
    }
}

/// Biographer abstains on a thin/young vault and runs once both the note count
/// (200) and age (60 days) thresholds are met.
#[test]
fn biographer_abstains_when_sparse_runs_when_rich() {
    let gate = SparseContentGate::default();

    // Thin: well under both thresholds → abstain with a reason.
    let thin = gate.should_abstain(snap(40, 20));
    assert!(thin.is_some(), "biographer must abstain on a thin vault");
    let reason = thin.unwrap();
    assert!(
        reason.contains("40") && reason.contains("200"),
        "abstain reason names the actual vs required note counts: {reason}"
    );

    // Enough notes but too young → still abstain (both must hold).
    assert!(
        gate.should_abstain(snap(500, 59)).is_some(),
        "biographer abstains when the vault is too young even with enough notes"
    );

    // Rich: at both thresholds → run (no abstain reason).
    assert!(
        gate.should_abstain(snap(200, 60)).is_none(),
        "biographer runs once 200 notes / 60 days are met"
    );
}

/// Annual Review abstains until the vault is twelve months old, then runs.
#[test]
fn annual_review_abstains_until_one_year_old() {
    let gate = MaturityGate::default();
    assert_eq!(
        gate.min_vault_age_days, 365,
        "design-doc threshold is 12 months"
    );

    assert!(
        gate.should_abstain(200).is_some(),
        "annual review abstains on a vault younger than a year"
    );
    assert!(
        gate.should_abstain(364).is_some(),
        "abstains right up to the boundary"
    );
    assert!(
        gate.should_abstain(365).is_none(),
        "runs once the vault is a full year old"
    );
}

/// Voice Keeper is tiered: observe-only below 50 human notes, propose-only from
/// 50, and mature only once it has 200 notes AND 30 days. The observe-only tier
/// is the sparse-content stance — it builds the voice model passively without
/// joining council or critiquing.
#[test]
fn voice_keeper_tiers_by_corpus_size_and_age() {
    let gate = VoiceKeeperBootstrap::default();

    // Thin: observe-only, out of council.
    let thin_tier = gate.tier(snap(20, 5));
    assert_eq!(thin_tier, VoiceKeeperTier::ObserveOnly);
    assert!(
        !thin_tier.participates_in_council(),
        "observe-only Voice Keeper does not join council"
    );
    assert!(
        gate.observe_only_reason(snap(20, 5)).is_some(),
        "observe-only tier surfaces a standup reason"
    );

    // Mid: propose-only — joins council, never auto-lands.
    let mid_tier = gate.tier(snap(120, 400));
    assert_eq!(mid_tier, VoiceKeeperTier::ProposeOnly);
    assert!(mid_tier.participates_in_council());
    assert!(
        !mid_tier.may_auto_land(),
        "propose-only rewrites are always reviewed"
    );
    assert!(
        gate.observe_only_reason(snap(120, 400)).is_none(),
        "no observe-only reason once Voice Keeper is active"
    );

    // Rich: mature — full design, may auto-land.
    let rich_tier = gate.tier(snap(250, 45));
    assert_eq!(rich_tier, VoiceKeeperTier::Mature);
    assert!(rich_tier.may_auto_land());
}

/// The three gates agree on a single thin fixture and a single rich fixture —
/// the property #137 asks for: on a thin vault every gated agent withholds, and
/// on a rich vault every gated agent is active. One snapshot, all agents.
#[test]
fn gates_agree_on_thin_and_rich_fixtures() {
    let bio = SparseContentGate::default();
    let annual = MaturityGate::default();
    let vk = VoiceKeeperBootstrap::default();

    // A brand-new vault: 12 notes, 8 days old.
    let thin = snap(12, 8);
    assert!(
        bio.should_abstain(thin).is_some(),
        "biographer withholds on thin"
    );
    assert!(
        annual.should_abstain(thin.age_days).is_some(),
        "annual review withholds on thin"
    );
    assert_eq!(
        vk.tier(thin),
        VoiceKeeperTier::ObserveOnly,
        "voice keeper observe-only on thin"
    );

    // A mature vault: 600 notes, 540 days (~18 months) old.
    let rich = snap(600, 540);
    assert!(
        bio.should_abstain(rich).is_none(),
        "biographer active on rich"
    );
    assert!(
        annual.should_abstain(rich.age_days).is_none(),
        "annual review active on rich"
    );
    assert_eq!(
        vk.tier(rich),
        VoiceKeeperTier::Mature,
        "voice keeper mature on rich"
    );
}
