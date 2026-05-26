//! Invasiveness classifier per `docs/design/01-agents-and-council.md`
//! §Invasiveness classifier.
//!
//! Classifies a proposed diff into one of four invasiveness classes
//! deterministically — no LLM call. Each agent's `max_invasiveness`
//! ceiling (set in its `config.toml`) gates whether the classifier's
//! verdict permits an autonomous write or forces a proposal up through
//! council / human review.
//!
//! # Slice scope
//!
//! This module ships the **enum**, the **input value type
//! ([`DiffSummary`])**, and the **pure `classify` function**.
//!
//! Out of scope for this slice (separate follow-ups):
//!
//! - The walker that turns a comrak markdown-AST diff into a
//!   `DiffSummary` — that's a substantial slice in its own right and
//!   depends on the AST representation we standardize on for diffs.
//! - Wiring `classify` into [`crate::runner::AgentRunner::run_agent`]
//!   alongside the existing confidence gate — that's the **decision
//!   matrix** slice of `#27`, which uses this module.
//! - Per-agent `max_invasiveness` configuration field on
//!   [`crate::runner::AgentConfig`] — landed by the decision-matrix
//!   slice.
//!
//! # Why a value type, not a trait
//!
//! The ADR pseudocode invokes helper methods on `Diff` (e.g.
//! `diff.modifies_frontmatter_fields(&["id", "type", "title"])`).
//! Modelling those as trait methods would force every test to define
//! a mock impl. Instead, the AST walker pre-computes the predicates
//! that depend on configured lists (the "critical frontmatter set",
//! the "safe additive kinds") and the classifier reads pre-computed
//! booleans. Same observable behaviour; testable with one-line struct
//! literals.

use serde::{Deserialize, Serialize};

/// The four invasiveness classes, ordered from least to most
/// disruptive per the spec table. Each agent's `max_invasiveness` is
/// compared against this verdict to decide auto-land vs. proposal.
///
/// Order is meaningful: `cmp` yields `Mechanical < Additive <
/// Editorial < Structural`, which lets a runner write
/// `if invasiveness <= agent.max_invasiveness { auto_land() }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Invasiveness {
    /// Pure cosmetic maintenance — tag dedup, frontmatter sort,
    /// trailing-whitespace trim. The lowest ceiling.
    Mechanical,
    /// Additive-only changes of safe kinds (new wikilinks, new
    /// section headings, HTML comments). No existing content modified.
    Additive,
    /// Modifies existing text, removes links/text blocks, or any
    /// additive change involving a non-safe kind. The default fallback
    /// when no other class is a clean match.
    Editorial,
    /// File creation/deletion, or modification of identity-critical
    /// frontmatter fields (`id`, `type`, `title`). Always requires
    /// human approval; never auto-lands.
    Structural,
}

impl Invasiveness {
    /// Stable string form, used by callers that persist the verdict
    /// (e.g. `agent_actions`, `proposals` rows). Matches `serde`'s
    /// `snake_case` rename.
    pub fn as_sql(&self) -> &'static str {
        match self {
            Self::Mechanical => "mechanical",
            Self::Additive => "additive",
            Self::Editorial => "editorial",
            Self::Structural => "structural",
        }
    }
}

/// Pre-computed predicates over a markdown-AST diff. Produced by the
/// AST walker (separate slice); consumed by [`classify`].
///
/// All fields default to `false` — an "empty diff" classifies as
/// `Editorial` (the safe fallback) because none of the
/// non-editorial branches match. Callers should compute the fields
/// strictly from the diff, not the source.
///
/// # Field meanings
///
/// - `creates_or_deletes_files`: any markdown file or sidecar JSON file
///   added or removed by this diff
/// - `modifies_critical_frontmatter`: any change to the frontmatter
///   fields `id`, `type`, or `title` (the identity-critical triad —
///   per the spec, these always count as structural)
/// - `removes_links`: any `[[wikilink]]` removed
/// - `removes_text_blocks`: any markdown block (paragraph, list,
///   code, etc.) removed in whole
/// - `modifies_existing_text_blocks`: any text inside an existing
///   block changed (insertion/deletion of characters within the
///   block; not the same as adding a new block)
/// - `adds_new_blocks_only`: the diff is non-empty and the *only*
///   changes are new blocks (no removals, no in-place modifications)
/// - `additive_only_safe_kinds`: when `adds_new_blocks_only` is true,
///   every added block is one of the safe-additive kinds
///   (`html_comment`, `wikilink`, `section_heading`). If
///   `adds_new_blocks_only` is false this field is ignored.
/// - `is_pure_metadata_normalization`: the only changes are tag
///   dedup, frontmatter key reordering, or trailing-whitespace trim
///   (the spec's three Mechanical-class kinds)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffSummary {
    /// File create or delete.
    pub creates_or_deletes_files: bool,
    /// Modifies `id` / `type` / `title` in frontmatter.
    pub modifies_critical_frontmatter: bool,
    /// Removes any wikilink.
    pub removes_links: bool,
    /// Removes whole markdown blocks.
    pub removes_text_blocks: bool,
    /// Modifies text inside existing blocks.
    pub modifies_existing_text_blocks: bool,
    /// Only adds new blocks (no removals, no in-place modifications);
    /// must be non-empty to be true.
    pub adds_new_blocks_only: bool,
    /// When `adds_new_blocks_only`, every added block is a safe kind
    /// (html_comment / wikilink / section_heading).
    pub additive_only_safe_kinds: bool,
    /// The only changes are cosmetic metadata normalization.
    pub is_pure_metadata_normalization: bool,
}

impl DiffSummary {
    /// An "empty" diff — no fields set. `classify` returns
    /// [`Invasiveness::Editorial`] (the safe fallback) because no
    /// branch matches an empty diff. Useful as a base in tests:
    /// `DiffSummary { creates_or_deletes_files: true, ..DiffSummary::empty() }`.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Classify a `DiffSummary` into one of four [`Invasiveness`] classes
/// per `01-agents-and-council.md`. Mirror of the ADR pseudocode; pure
/// function, deterministic.
///
/// Branch order matters — the spec evaluates `Structural` triggers
/// before `Editorial` before `Additive` before `Mechanical`, falling
/// through to `Editorial` as the safe default. The `Mechanical` branch
/// runs *last* because pure metadata normalization that also happens
/// to touch identity-critical frontmatter should still be `Structural`
/// (the broader concern wins).
pub fn classify(diff: &DiffSummary) -> Invasiveness {
    if diff.creates_or_deletes_files {
        return Invasiveness::Structural;
    }
    if diff.modifies_critical_frontmatter {
        return Invasiveness::Structural;
    }
    if diff.removes_links || diff.removes_text_blocks {
        return Invasiveness::Editorial;
    }
    if diff.modifies_existing_text_blocks {
        return Invasiveness::Editorial;
    }
    if diff.adds_new_blocks_only {
        if diff.additive_only_safe_kinds {
            return Invasiveness::Additive;
        }
        return Invasiveness::Editorial;
    }
    if diff.is_pure_metadata_normalization {
        return Invasiveness::Mechanical;
    }
    // Safe fallback. An empty diff or a diff whose flags don't match
    // any branch above is Editorial — better to require council than
    // to silently auto-land an unclassifiable change.
    Invasiveness::Editorial
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test corresponds to one branch of the classifier so a
    // regression in any single rule trips a single, named test rather
    // than a generic one.

    #[test]
    fn file_create_or_delete_is_structural() {
        let d = DiffSummary {
            creates_or_deletes_files: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Structural);
    }

    #[test]
    fn critical_frontmatter_change_is_structural() {
        let d = DiffSummary {
            modifies_critical_frontmatter: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Structural);
    }

    #[test]
    fn link_removal_is_editorial() {
        let d = DiffSummary {
            removes_links: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Editorial);
    }

    #[test]
    fn text_block_removal_is_editorial() {
        let d = DiffSummary {
            removes_text_blocks: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Editorial);
    }

    #[test]
    fn in_place_modification_is_editorial() {
        let d = DiffSummary {
            modifies_existing_text_blocks: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Editorial);
    }

    #[test]
    fn purely_additive_safe_kinds_is_additive() {
        let d = DiffSummary {
            adds_new_blocks_only: true,
            additive_only_safe_kinds: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Additive);
    }

    #[test]
    fn additive_with_unsafe_kinds_is_editorial() {
        // e.g. adding a new code block — additive but the kind is
        // outside the safe set, so it merits an editorial review.
        let d = DiffSummary {
            adds_new_blocks_only: true,
            additive_only_safe_kinds: false,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Editorial);
    }

    #[test]
    fn pure_metadata_normalization_is_mechanical() {
        let d = DiffSummary {
            is_pure_metadata_normalization: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Mechanical);
    }

    #[test]
    fn empty_diff_defaults_to_editorial() {
        // Belt-and-braces: an empty diff should never be classified
        // as Mechanical (which would auto-land); the safe fallback is
        // Editorial.
        assert_eq!(classify(&DiffSummary::empty()), Invasiveness::Editorial);
    }

    #[test]
    fn structural_trumps_normalization() {
        // A diff that's pure metadata normalization *and* touches
        // `id`/`type`/`title` must be Structural, not Mechanical —
        // the broader concern wins. Tests the branch ordering.
        let d = DiffSummary {
            modifies_critical_frontmatter: true,
            is_pure_metadata_normalization: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Structural);
    }

    #[test]
    fn structural_trumps_link_removal() {
        let d = DiffSummary {
            creates_or_deletes_files: true,
            removes_links: true,
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Structural);
    }

    #[test]
    fn editorial_trumps_additive_when_blocks_are_removed_too() {
        // Spec branch order: removes_text_blocks check runs before
        // adds_new_blocks_only. A diff that both adds and removes is
        // Editorial.
        let d = DiffSummary {
            removes_text_blocks: true,
            adds_new_blocks_only: false, // adds_new_blocks_only is false because there's also a removal
            ..DiffSummary::empty()
        };
        assert_eq!(classify(&d), Invasiveness::Editorial);
    }

    #[test]
    fn ordering_for_max_invasiveness_comparison() {
        // The runner will write `verdict <= agent.max_invasiveness`;
        // pin the ordering so a future refactor can't silently flip
        // Mechanical above Structural and accidentally let agents
        // auto-land file deletions.
        assert!(Invasiveness::Mechanical < Invasiveness::Additive);
        assert!(Invasiveness::Additive < Invasiveness::Editorial);
        assert!(Invasiveness::Editorial < Invasiveness::Structural);
    }

    #[test]
    fn as_sql_is_snake_case_and_stable() {
        // Persisted in agent_actions / proposals; the strings are
        // contract-stable.
        assert_eq!(Invasiveness::Mechanical.as_sql(), "mechanical");
        assert_eq!(Invasiveness::Additive.as_sql(), "additive");
        assert_eq!(Invasiveness::Editorial.as_sql(), "editorial");
        assert_eq!(Invasiveness::Structural.as_sql(), "structural");
    }
}
