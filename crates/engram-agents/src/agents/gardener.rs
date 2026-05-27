//! Typed output schema for the Gardener agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/gardener/prompt.md` § "Output schema". Per ADR 0011,
//! `confidence` and `rationale` come first so streaming early-exit
//! can abort before the removals and flags arrays.
//!
//! The Gardener follows the ADR 0013 pre-filter-then-judge pattern:
//! deterministic code ([`find_dead_links`], [`find_todo_candidates`])
//! finds candidates; the LLM decides which to act on. This keeps
//! token usage low and makes the LLM judgment auditable.
//!
//! ## Invasiveness tiers
//!
//! - Dead-link removals: `DeadLink` confidence is near-deterministic
//!   (0.99); auto-lands at `auto_land_min_confidence = 0.90`.
//! - Resolved-TODO removals: confidence is LLM self-scored.
//! - Stale-note flags: advisory only; no write occurs.

use serde::{Deserialize, Serialize};

/// Top-level output from the Gardener agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `removals` → `flags`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GardenerOutput {
    /// Self-assessed confidence (0.0–1.0) for the combined set of
    /// removals and flags. Dead-link removals are near-deterministic
    /// (0.99); TODO removals are LLM-self-scored; flags are advisory.
    pub confidence: f32,

    /// One paragraph explaining what was pruned or flagged and why.
    pub rationale: String,

    /// Dead wikilinks or resolved TODOs to remove from notes.
    /// Defaults to an empty vec when the LLM finds nothing to prune.
    #[serde(default)]
    pub removals: Vec<GardenerRemoval>,

    /// Notes that show signs of decay (stale, no incoming links, etc.)
    /// and should receive an `engram/needs-review` tag.
    #[serde(default)]
    pub flags: Vec<GardenerFlag>,
}

/// A single removal — either a dead wikilink or a resolved TODO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GardenerRemoval {
    /// The ULID/slug of the note that contains the dead link or
    /// resolved TODO.
    pub note_id: String,

    /// Whether this is a dead wikilink or a resolved TODO checkbox.
    pub kind: RemovalKind,

    /// The wikilink target title or the TODO item text.
    pub target: String,

    /// Per-removal confidence (0.0–1.0). Dead-link removals should
    /// be 0.99; TODO removals are LLM-self-scored.
    pub confidence: f32,

    /// One sentence explaining why this removal is safe, referencing
    /// the specific evidence (e.g. "[[Foo]] has no matching note in
    /// the vault as of this run").
    pub provenance_comment: String,
}

/// Discriminates between the two removal kinds the Gardener handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    /// The `[[Target]]` wikilink points to a note title that does
    /// not exist in the vault.
    DeadLink,
    /// The `- [ ] …` TODO checkbox item has been resolved
    /// (the LLM judged that the work is done or no longer relevant).
    ResolvedTodo,
}

/// A note flagged as potentially stale or decayed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GardenerFlag {
    /// The ULID/slug of the note to flag.
    pub note_id: String,

    /// Human-readable reason for the flag, e.g.
    /// `"stale: no incoming links, 2 years old"`.
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Deterministic pre-filter helpers (ADR 0013: tool-use over generation)
// ---------------------------------------------------------------------------

/// Find dead wikilinks in a note's body.
///
/// Scans `body` for `[[Target]]` patterns and returns the targets
/// that are **not** present in `known_titles`. Comparison is
/// case-sensitive and whitespace-trimmed to match vault filename
/// conventions (ADR 0006: pure title-slug filenames).
///
/// Returns a `Vec<String>` of dead link targets (the text between
/// `[[` and `]]`). Duplicates are preserved — if the same dead link
/// appears twice the caller can decide how to deduplicate.
pub fn find_dead_links(
    body: &str,
    known_titles: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut dead = Vec::new();
    let mut remaining = body;
    while let Some(open) = remaining.find("[[") {
        remaining = &remaining[open + 2..];
        if let Some(close) = remaining.find("]]") {
            let target = remaining[..close].trim();
            if !target.is_empty() && !known_titles.contains(target) {
                dead.push(target.to_string());
            }
            remaining = &remaining[close + 2..];
        } else {
            // No closing `]]`; malformed — stop scanning.
            break;
        }
    }
    dead
}

/// Extract open (unchecked) TODO items from a note's body.
///
/// Matches GFM-style `- [ ]` checkboxes (open) and returns the text
/// of each unchecked item, trimmed. Does **not** return checked
/// (`- [x]`) or partial-progress items.
pub fn find_todo_candidates(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            // Match `- [ ] <text>` (case-sensitive `[ ]` per GFM spec)
            if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
                Some(rest.trim().to_string())
            } else {
                None
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ── Serde round-trip ────────────────────────────────────────────────────

    #[test]
    fn gardener_output_round_trips_via_serde_json() {
        let original = GardenerOutput {
            confidence: 0.95,
            rationale: "Removed two dead links and flagged one stale note.".into(),
            removals: vec![
                GardenerRemoval {
                    note_id: "rust-ownership".into(),
                    kind: RemovalKind::DeadLink,
                    target: "Nonexistent Note".into(),
                    confidence: 0.99,
                    provenance_comment: "[[Nonexistent Note]] has no matching note in the vault."
                        .into(),
                },
                GardenerRemoval {
                    note_id: "project-ideas".into(),
                    kind: RemovalKind::ResolvedTodo,
                    target: "Write the gardener spec".into(),
                    confidence: 0.85,
                    provenance_comment: "The spec was merged; this TODO is done.".into(),
                },
            ],
            flags: vec![GardenerFlag {
                note_id: "old-fleeting-note".into(),
                reason: "stale: no incoming links, 2 years old".into(),
            }],
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: GardenerOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn empty_removals_and_flags_default_to_empty_vec() {
        let json = r#"{"confidence":0.5,"rationale":"nothing to do"}"#;
        let parsed: GardenerOutput = serde_json::from_str(json).expect("parse");
        assert!(parsed.removals.is_empty());
        assert!(parsed.flags.is_empty());
    }

    #[test]
    fn unknown_field_rejected() {
        let json = r#"{"confidence":0.5,"rationale":"r","future_field":"x"}"#;
        let err =
            serde_json::from_str::<GardenerOutput>(json).expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error should name the offending field; got: {err}"
        );
    }

    // ── find_dead_links ─────────────────────────────────────────────────────

    #[test]
    fn find_dead_links_returns_link_not_in_known_titles() {
        let known: HashSet<String> = ["Rust Ownership".to_string()].into();
        let body = "See [[Missing Note]] for details.";
        let dead = find_dead_links(body, &known);
        assert_eq!(dead, vec!["Missing Note"]);
    }

    #[test]
    fn find_dead_links_does_not_return_live_link() {
        let known: HashSet<String> = ["Rust Ownership".to_string()].into();
        let body = "See [[Rust Ownership]] for details.";
        let dead = find_dead_links(body, &known);
        assert!(dead.is_empty(), "live link must not appear in dead list");
    }

    #[test]
    fn find_dead_links_returns_only_dead_among_multiple() {
        let known: HashSet<String> = ["Live Note".to_string(), "Another Live".to_string()].into();
        let body = "[[Live Note]], [[Dead One]], [[Another Live]], [[Also Dead]]";
        let dead = find_dead_links(body, &known);
        assert_eq!(dead, vec!["Dead One", "Also Dead"]);
    }

    #[test]
    fn find_dead_links_empty_body_returns_empty() {
        let known: HashSet<String> = ["Anything".to_string()].into();
        let dead = find_dead_links("", &known);
        assert!(dead.is_empty());
    }

    // ── find_todo_candidates ────────────────────────────────────────────────

    #[test]
    fn find_todo_candidates_extracts_open_item() {
        let body = "- [ ] Do something important\n";
        let todos = find_todo_candidates(body);
        assert_eq!(todos, vec!["Do something important"]);
    }

    #[test]
    fn find_todo_candidates_ignores_checked_item() {
        let body = "- [x] Done item\n- [ ] Still open\n";
        let todos = find_todo_candidates(body);
        assert_eq!(todos, vec!["Still open"]);
    }

    #[test]
    fn find_todo_candidates_empty_body_returns_empty() {
        let todos = find_todo_candidates("");
        assert!(todos.is_empty());
    }

    // ── ADR 0011 streaming-order contract ───────────────────────────────────

    #[test]
    fn serializes_confidence_before_rationale_before_payload() {
        let out = GardenerOutput {
            confidence: 0.9,
            rationale: "r".into(),
            removals: vec![],
            flags: vec![],
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("confidence present");
        let rat_idx = json.find("\"rationale\"").expect("rationale present");
        let rem_idx = json.find("\"removals\"").expect("removals present");
        assert!(
            conf_idx < rat_idx && rat_idx < rem_idx,
            "field order must be confidence < rationale < removals (got {conf_idx}, {rat_idx}, {rem_idx})"
        );
    }

    // ── RemovalKind serde ───────────────────────────────────────────────────

    #[test]
    fn removal_kind_snake_case_round_trips() {
        for (json_val, expected) in [
            ("dead_link", RemovalKind::DeadLink),
            ("resolved_todo", RemovalKind::ResolvedTodo),
        ] {
            let parsed: RemovalKind =
                serde_json::from_str(&format!("\"{json_val}\"")).expect("parse");
            assert_eq!(parsed, expected);
        }
    }
}
