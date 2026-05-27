//! Cartographer agent — keeps `index.md` (and optionally MOC files) in sync
//! with the live vault.
//!
//! ## Modes
//!
//! * **Continuous** (`CartographerContinuousOutput`) — triggered on file
//!   change (or cron). Receives a short list of recently-changed notes and
//!   emits targeted add/update/remove operations against `index.md`.
//! * **Quarterly audit** (`CartographerAuditOutput`) — full-vault scan;
//!   proposes tag renames, merges, and hierarchy changes for human review.
//!   Never auto-lands (always filed as a proposal).
//!
//! ## Karpathy index format
//!
//! ```text
//! - [[Title]]: <one-sentence summary ≤ 20 words>
//! ```
//!
//! Entries are sorted by note type first (from frontmatter `type:` field),
//! then alphabetically by title within each type.
//!
//! ## Confidence formula (continuous mode)
//!
//! 1. Start with the LLM's self-reported `confidence` score (0.0–1.0).
//! 2. For each `add` / `update` operation whose summary shares ≥ 1 word
//!    (case-insensitive, ignoring stop-words) with the note body, add 0.1
//!    (capped at 1.0).
//!
//! ## Auto-land gate (continuous mode)
//!
//! * `add` / `update` operations with `confidence ≥ agent.confidence_threshold`
//!   → write unstaged directly.
//! * `remove` operations on notes that still exist in the vault → file as
//!   council proposal regardless of confidence (removal is destructive).
//! * Everything else → file as proposal.
//!
//! ## References
//!
//! * `docs/design/12-agent-spec-template.md` §Cartographer
//! * Issue [#43](https://github.com/torsday/engram/issues/43)

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ────────────────────────────────────────────────────────────────
// Continuous mode types
// ────────────────────────────────────────────────────────────────

/// A single `add`, `update`, or `remove` operation against `index.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexUpdate {
    /// The operation to perform.
    pub op: IndexOp,
    /// Note title (used as the `[[wikilink]]` key).
    pub title: String,
    /// The one-sentence summary (≤ 20 words). Required for `add` / `update`;
    /// ignored for `remove`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Which operation an [`IndexUpdate`] performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexOp {
    /// Insert a new entry into `index.md`.
    Add,
    /// Replace an existing entry's summary in `index.md`.
    Update,
    /// Delete an existing entry from `index.md`.
    Remove,
}

/// Structured output produced by the Cartographer in **continuous mode**.
///
/// The LLM is expected to return a JSON object that deserialises into this
/// struct.  The runner validates `confidence` ∈ [0, 1] and adjusts it with
/// the word-overlap check before applying the auto-land gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartographerContinuousOutput {
    /// LLM self-reported confidence in [0.0, 1.0].
    pub confidence: f32,
    /// Short human-readable explanation of what the LLM did and why.
    pub rationale: String,
    /// Ordered list of index mutations to apply.
    pub index_updates: Vec<IndexUpdate>,
}

// ────────────────────────────────────────────────────────────────
// Quarterly audit mode types
// ────────────────────────────────────────────────────────────────

/// A tag rename / merge / hierarchy change proposal from the quarterly audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagProposal {
    /// Kind of tag change being proposed.
    pub kind: TagProposalKind,
    /// Source tag (or the tag being renamed / merged from).
    pub from_tag: String,
    /// Target tag name (for renames and merges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_tag: Option<String>,
    /// Optional parent tag for hierarchy proposals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tag: Option<String>,
    /// Human-readable justification.
    pub rationale: String,
    /// Number of notes affected.
    pub affected_count: u32,
}

/// What kind of structural change a [`TagProposal`] suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagProposalKind {
    /// Rename `from_tag` to `to_tag`.
    Rename,
    /// Merge `from_tag` into `to_tag`.
    Merge,
    /// Place `from_tag` under `parent_tag` in the hierarchy.
    Hierarchy,
}

/// Structured output produced by the Cartographer in **quarterly audit mode**.
///
/// Quarterly audit output always lands as a council proposal — never
/// auto-applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartographerAuditOutput {
    /// LLM self-reported confidence in [0.0, 1.0].
    pub confidence: f32,
    /// Brief summary of findings.
    pub summary: String,
    /// Tag restructuring proposals for human review.
    pub tag_proposals: Vec<TagProposal>,
}

// ────────────────────────────────────────────────────────────────
// Index rendering / parsing helpers
// ────────────────────────────────────────────────────────────────

/// Common English stop-words excluded from the word-overlap confidence check.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "was", "are", "were", "be", "been", "being", "have", "has", "had", "do", "does",
    "did", "will", "would", "could", "should", "may", "might", "shall", "can", "it", "its", "this",
    "that", "these", "those", "as", "not", "no",
];

/// Apply the word-overlap bonus to a base confidence score.
///
/// For each `add` / `update` operation, if the summary shares ≥ 1
/// meaningful word (case-insensitive, stop-words excluded) with the
/// `note_body`, add 0.1 (capped at 1.0).
pub fn adjust_confidence(
    base_confidence: f32,
    updates: &[IndexUpdate],
    note_bodies: &[(String, String)], // (title, body) pairs
) -> f32 {
    let stop: HashSet<&str> = STOP_WORDS.iter().copied().collect();

    let mut conf = base_confidence;

    for update in updates {
        if matches!(update.op, IndexOp::Remove) {
            continue;
        }
        let Some(summary) = &update.summary else {
            continue;
        };

        // Find the matching note body by title (case-insensitive).
        let body = note_bodies
            .iter()
            .find(|(t, _)| t.to_lowercase() == update.title.to_lowercase())
            .map(|(_, b)| b.as_str())
            .unwrap_or("");

        let summary_words: HashSet<String> = summary
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_owned()
            })
            .filter(|w| !w.is_empty() && !stop.contains(w.as_str()))
            .collect();

        let body_words: HashSet<String> = body
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_owned()
            })
            .filter(|w| !w.is_empty() && !stop.contains(w.as_str()))
            .collect();

        if summary_words.iter().any(|w| body_words.contains(w)) {
            conf = (conf + 0.1).min(1.0);
        }
    }

    conf
}

/// Parse an existing `index.md` file into a list of `(title, summary)` pairs.
///
/// Lines that don't match the Karpathy format are preserved verbatim as
/// header/footer lines so the file is round-tripped faithfully.
pub fn parse_index(content: &str) -> IndexFile {
    let mut entries: Vec<IndexEntry> = Vec::new();
    let mut header: Vec<String> = Vec::new();
    let mut footer: Vec<String> = Vec::new();
    let mut in_entries = false;
    let mut after_entries = false;

    for line in content.lines() {
        if after_entries {
            footer.push(line.to_owned());
            continue;
        }

        if let Some(entry) = parse_entry_line(line) {
            in_entries = true;
            entries.push(entry);
        } else if in_entries {
            // First non-entry line after entries started → footer
            after_entries = true;
            footer.push(line.to_owned());
        } else {
            header.push(line.to_owned());
        }
    }

    IndexFile {
        header,
        entries,
        footer,
    }
}

/// Render an [`IndexFile`] back to a string.
pub fn render_index(index: &IndexFile) -> String {
    let mut out = String::new();

    for h in &index.header {
        out.push_str(h);
        out.push('\n');
    }
    for entry in &index.entries {
        out.push_str(&format!("- [[{}]]: {}\n", entry.title, entry.summary));
    }
    for f in &index.footer {
        out.push_str(f);
        out.push('\n');
    }

    // Trim trailing newline added by the last footer line if content had none
    out
}

/// Apply a list of [`IndexUpdate`] operations to an [`IndexFile`] in memory.
///
/// Returns the path (relative to vault root) of notes that were targeted by
/// `remove` operations AND whose corresponding vault file still exists.  The
/// caller should treat those as proposal-only (no auto-land).
pub fn apply_updates(
    index: &mut IndexFile,
    updates: &[IndexUpdate],
    vault_root: &Path,
) -> Vec<String> {
    let mut council_titles: Vec<String> = Vec::new();

    for update in updates {
        match update.op {
            IndexOp::Add => {
                // Only add if not already present.
                if !index.entries.iter().any(|e| e.title == update.title) {
                    let summary = update.summary.clone().unwrap_or_default();
                    index.entries.push(IndexEntry {
                        title: update.title.clone(),
                        summary,
                    });
                }
            }
            IndexOp::Update => {
                if let Some(entry) = index.entries.iter_mut().find(|e| e.title == update.title) {
                    if let Some(summary) = &update.summary {
                        entry.summary = summary.clone();
                    }
                }
            }
            IndexOp::Remove => {
                // Check if the vault file still exists — if so, punt to council.
                let slug = engram_core::slug::slugify(&update.title);
                let note_path = vault_root.join(format!("{}.md", slug));
                if note_path.exists() {
                    council_titles.push(update.title.clone());
                } else {
                    index.entries.retain(|e| e.title != update.title);
                }
            }
        }
    }

    // Re-sort: stable sort preserves within-group order, which the LLM may
    // have set intentionally; we only enforce the type → alpha order here
    // for entries that carry a `note_type` annotation.  For now sort
    // entirely alphabetically — a follow-up can add type-aware ordering
    // once the vault's `type:` frontmatter is threaded through.
    index.entries.sort_by(|a, b| a.title.cmp(&b.title));

    council_titles
}

fn parse_entry_line(line: &str) -> Option<IndexEntry> {
    // Pattern: `- [[Title]]: summary text`
    let line = line.trim();
    let rest = line.strip_prefix("- [[")?;
    let (title, rest) = rest.split_once("]]:")?;
    let summary = rest.trim().to_owned();
    Some(IndexEntry {
        title: title.to_owned(),
        summary,
    })
}

/// Parsed representation of an `index.md` file.
#[derive(Debug, Clone, Default)]
pub struct IndexFile {
    /// Lines before the first entry (e.g. YAML front-matter, title heading).
    pub header: Vec<String>,
    /// The Karpathy-format entries.
    pub entries: Vec<IndexEntry>,
    /// Lines after the last entry (e.g. footnotes, blank trailing line).
    pub footer: Vec<String>,
}

/// A single entry in an `index.md` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Wikilink title (no brackets).
    pub title: String,
    /// One-sentence summary (≤ 20 words per spec).
    pub summary: String,
}

/// Returns the default path for `index.md` within a vault root.
pub fn index_path(vault_root: &Path) -> PathBuf {
    vault_root.join("index.md")
}

// ────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn vault_root() -> PathBuf {
        PathBuf::from("/nonexistent/vault")
    }

    // ── parse / render round-trip ────────────────────────────────

    #[test]
    fn parse_empty_index() {
        let idx = parse_index("");
        assert!(idx.entries.is_empty());
        assert!(idx.header.is_empty());
    }

    #[test]
    fn parse_entries_only() {
        let content = "- [[Note A]]: Summary for A.\n- [[Note B]]: Summary for B.\n";
        let idx = parse_index(content);
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.entries[0].title, "Note A");
        assert_eq!(idx.entries[0].summary, "Summary for A.");
        assert_eq!(idx.entries[1].title, "Note B");
    }

    #[test]
    fn parse_with_header_and_footer() {
        let content = "# Index\n\n- [[Note A]]: Summary.\n\nFooter line.\n";
        let idx = parse_index(content);
        assert_eq!(idx.header, vec!["# Index", ""]);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.footer, vec!["", "Footer line."]);
    }

    #[test]
    fn render_round_trip() {
        let content = "# Index\n\n- [[Note A]]: Summary A.\n- [[Note B]]: Summary B.\n";
        let idx = parse_index(content);
        let rendered = render_index(&idx);
        assert_eq!(rendered, content);
    }

    // ── apply_updates ────────────────────────────────────────────

    #[test]
    fn add_new_entry() {
        let mut idx = parse_index("- [[Existing]]: Old summary.\n");
        let updates = vec![IndexUpdate {
            op: IndexOp::Add,
            title: "New Note".to_owned(),
            summary: Some("A brand new note.".to_owned()),
        }];
        let council = apply_updates(&mut idx, &updates, &vault_root());
        assert!(council.is_empty());
        assert_eq!(idx.entries.len(), 2);
        // entries sorted alpha: "Existing" < "New Note"
        assert_eq!(idx.entries[0].title, "Existing");
        assert_eq!(idx.entries[1].title, "New Note");
    }

    #[test]
    fn add_does_not_duplicate() {
        let mut idx = parse_index("- [[Note A]]: Summary.\n");
        let updates = vec![IndexUpdate {
            op: IndexOp::Add,
            title: "Note A".to_owned(),
            summary: Some("Different.".to_owned()),
        }];
        apply_updates(&mut idx, &updates, &vault_root());
        assert_eq!(idx.entries.len(), 1);
        // Original summary preserved
        assert_eq!(idx.entries[0].summary, "Summary.");
    }

    #[test]
    fn update_existing_entry() {
        let mut idx = parse_index("- [[Note A]]: Old summary.\n");
        let updates = vec![IndexUpdate {
            op: IndexOp::Update,
            title: "Note A".to_owned(),
            summary: Some("New summary.".to_owned()),
        }];
        apply_updates(&mut idx, &updates, &vault_root());
        assert_eq!(idx.entries[0].summary, "New summary.");
    }

    #[test]
    fn remove_nonexistent_vault_file_removes_entry() {
        let mut idx = parse_index("- [[Ghost Note]]: Gone.\n");
        let updates = vec![IndexUpdate {
            op: IndexOp::Remove,
            title: "Ghost Note".to_owned(),
            summary: None,
        }];
        // vault_root points to /nonexistent so the file won't exist
        let council = apply_updates(&mut idx, &updates, &vault_root());
        assert!(
            council.is_empty(),
            "should auto-remove when vault file absent"
        );
        assert!(idx.entries.is_empty());
    }

    // ── confidence adjustment ─────────────────────────────────────

    #[test]
    fn confidence_unchanged_when_no_matching_words() {
        let updates = vec![IndexUpdate {
            op: IndexOp::Add,
            title: "Cats".to_owned(),
            summary: Some("Felines that purr and sleep.".to_owned()),
        }];
        let bodies = vec![("Cats".to_owned(), "Dogs bark loudly.".to_owned())];
        let conf = adjust_confidence(0.7, &updates, &bodies);
        assert!((conf - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn confidence_boosted_when_words_overlap() {
        let updates = vec![IndexUpdate {
            op: IndexOp::Add,
            title: "Rust".to_owned(),
            summary: Some("Memory-safe systems programming language.".to_owned()),
        }];
        let bodies = vec![(
            "Rust".to_owned(),
            "Rust is a memory-safe language for systems programming.".to_owned(),
        )];
        let conf = adjust_confidence(0.7, &updates, &bodies);
        assert!(conf > 0.7, "expected boost for overlapping words");
        assert!(conf <= 1.0);
    }

    #[test]
    fn confidence_capped_at_one() {
        let updates = vec![
            IndexUpdate {
                op: IndexOp::Add,
                title: "A".to_owned(),
                summary: Some("alpha beta gamma".to_owned()),
            },
            IndexUpdate {
                op: IndexOp::Add,
                title: "B".to_owned(),
                summary: Some("delta epsilon zeta".to_owned()),
            },
            IndexUpdate {
                op: IndexOp::Add,
                title: "C".to_owned(),
                summary: Some("eta theta iota".to_owned()),
            },
            IndexUpdate {
                op: IndexOp::Add,
                title: "D".to_owned(),
                summary: Some("kappa lambda mu".to_owned()),
            },
        ];
        let bodies = vec![
            ("A".to_owned(), "alpha beta gamma".to_owned()),
            ("B".to_owned(), "delta epsilon zeta".to_owned()),
            ("C".to_owned(), "eta theta iota".to_owned()),
            ("D".to_owned(), "kappa lambda mu".to_owned()),
        ];
        let conf = adjust_confidence(0.9, &updates, &bodies);
        assert_eq!(conf, 1.0);
    }

    #[test]
    fn remove_op_does_not_affect_confidence() {
        let updates = vec![IndexUpdate {
            op: IndexOp::Remove,
            title: "Old Note".to_owned(),
            summary: None,
        }];
        let conf = adjust_confidence(0.5, &updates, &[]);
        assert!((conf - 0.5).abs() < f32::EPSILON);
    }

    // ── JSON round-trip ───────────────────────────────────────────

    #[test]
    fn continuous_output_serde_round_trip() {
        let output = CartographerContinuousOutput {
            confidence: 0.85,
            rationale: "Added three new notes.".to_owned(),
            index_updates: vec![
                IndexUpdate {
                    op: IndexOp::Add,
                    title: "New Note".to_owned(),
                    summary: Some("A short summary.".to_owned()),
                },
                IndexUpdate {
                    op: IndexOp::Remove,
                    title: "Stale Note".to_owned(),
                    summary: None,
                },
            ],
        };
        let json = serde_json::to_string(&output).unwrap();
        let decoded: CartographerContinuousOutput = serde_json::from_str(&json).unwrap();
        assert!((decoded.confidence - 0.85).abs() < f32::EPSILON);
        assert_eq!(decoded.index_updates.len(), 2);
        assert_eq!(decoded.index_updates[0].op, IndexOp::Add);
        assert!(decoded.index_updates[1].summary.is_none());
    }

    #[test]
    fn audit_output_serde_round_trip() {
        let output = CartographerAuditOutput {
            confidence: 0.9,
            summary: "Quarterly pass found 3 proposals.".to_owned(),
            tag_proposals: vec![TagProposal {
                kind: TagProposalKind::Rename,
                from_tag: "ml".to_owned(),
                to_tag: Some("machine-learning".to_owned()),
                parent_tag: None,
                rationale: "Prefer full name.".to_owned(),
                affected_count: 12,
            }],
        };
        let json = serde_json::to_string(&output).unwrap();
        let decoded: CartographerAuditOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.tag_proposals[0].kind, TagProposalKind::Rename);
        assert_eq!(decoded.tag_proposals[0].from_tag, "ml");
    }
}
