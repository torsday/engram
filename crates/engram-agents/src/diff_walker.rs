//! Conservative text-level walker that produces a [`DiffSummary`]
//! from a before/after content pair.
//!
//! This is the producer side of the [classifier
//! pipeline](crate::invasiveness). The full markdown-AST walker
//! (which inspects wikilinks, frontmatter, and block kinds) is a
//! separate slice; this module covers the easy text-level cases so
//! the runner's decision matrix (#218) has a working producer to
//! consume:
//!
//! - identical content → empty diff → classifies as `Editorial` (safe
//!   fallback) — see [`classify`](crate::invasiveness::classify)
//! - pure insertions (every line in `after` either matches a line in
//!   `before` *or* is new; no `before` line is missing) → sets
//!   `adds_new_blocks_only = true` so the classifier yields `Editorial`
//!   (we don't claim `additive_only_safe_kinds` from a text walker —
//!   that requires AST-level kind detection)
//! - pure whitespace normalization (trailing whitespace + blank-line
//!   collapsing only; the lines' trimmed content matches in order) →
//!   sets `is_pure_metadata_normalization = true` so the classifier
//!   yields `Mechanical`
//! - everything else → `modifies_existing_text_blocks = true` →
//!   classifies as `Editorial`
//!
//! # What this walker deliberately doesn't do
//!
//! - **`creates_or_deletes_files`** — the walker doesn't see paths.
//!   Callers compute this from the file-set delta and OR it onto the
//!   returned summary themselves.
//! - **`modifies_critical_frontmatter`** — requires YAML frontmatter
//!   parsing + a field-set comparison; deferred to the AST walker.
//! - **`removes_links`** — requires wikilink extraction; deferred.
//! - **`additive_only_safe_kinds`** — requires markdown block-kind
//!   detection; deferred. We leave this `false` so additive diffs
//!   conservatively classify as `Editorial` (council review) rather
//!   than `Additive` (auto-land) until the AST walker can confirm
//!   safety.
//!
//! The conservative bias is intentional: this walker errs toward
//! "needs review" so unsafe diffs never auto-land just because they
//! look pure-additive at the line level.

use crate::invasiveness::DiffSummary;

/// Summarize the diff between `before` and `after` at the line level.
///
/// See the module docs for the full set of cases handled and explicitly
/// deferred. Returns [`DiffSummary::empty`] when `before == after`.
pub fn summarize_text_diff(before: &str, after: &str) -> DiffSummary {
    if before == after {
        return DiffSummary::empty();
    }

    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // Case 1: pure whitespace normalization.
    //
    // The content trimmed of trailing whitespace, with consecutive
    // blank lines collapsed, must be identical on both sides — and
    // there must be at least one whitespace-only difference for this
    // to be a *change* (we already returned for identical input).
    if is_pure_whitespace_normalization(&before_lines, &after_lines) {
        return DiffSummary {
            is_pure_metadata_normalization: true,
            ..DiffSummary::empty()
        };
    }

    // Case 2: pure insertion (every `before` line appears in `after`
    // in order; `after` may have additional lines interleaved).
    //
    // This is the "agent appended a section" pattern. Conservative on
    // kind safety — we don't claim `additive_only_safe_kinds` from
    // line-level analysis.
    if is_pure_insertion(&before_lines, &after_lines) {
        return DiffSummary {
            adds_new_blocks_only: true,
            ..DiffSummary::empty()
        };
    }

    // Default: text inside an existing block changed (or content was
    // removed, etc.). The classifier maps this to Editorial — the
    // safe fallback that forces council review.
    DiffSummary {
        modifies_existing_text_blocks: true,
        ..DiffSummary::empty()
    }
}

/// True iff the two slices are equal modulo trailing whitespace +
/// consecutive-blank-line collapsing.
fn is_pure_whitespace_normalization(before: &[&str], after: &[&str]) -> bool {
    let canon_before: Vec<&str> = canonicalize_whitespace(before);
    let canon_after: Vec<&str> = canonicalize_whitespace(after);
    canon_before == canon_after
}

/// Trim trailing whitespace per line and collapse consecutive blank
/// lines into a single blank line. Mirrors the normalization rules
/// the embedding pipeline uses for `content_hash` (see
/// `engram_index::embeddings::normalize_for_hash`).
fn canonicalize_whitespace<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = Vec::with_capacity(lines.len());
    let mut last_was_blank = false;
    for line in lines {
        let trimmed = line.trim_end();
        let is_blank = trimmed.is_empty();
        if is_blank && last_was_blank {
            continue;
        }
        // Keep the original `&str` slice if its trim is the same byte
        // range, otherwise fall back to a fresh allocation via the
        // outer `Vec<String>` path. Simplest: use the trimmed slice
        // when length matches, else use the trimmed string directly.
        // Since we only compare for equality, the actual storage form
        // doesn't matter — use the trimmed slice consistently.
        out.push(trimmed);
        last_was_blank = is_blank;
    }
    // Remove trailing blank lines so a file ending in extra blank
    // lines normalizes to the same form as one without.
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// True iff `after` contains every line of `before` in the same
/// relative order — i.e., the diff consists only of insertions.
///
/// Implementation: two-pointer walk. For each `before` line, advance
/// the `after` pointer until we find a matching line; if we exhaust
/// `after` without finding it, the line was removed (or reordered)
/// and this is not a pure insertion.
fn is_pure_insertion(before: &[&str], after: &[&str]) -> bool {
    if after.len() <= before.len() {
        // Pure insertion requires `after` to be strictly longer (we
        // already returned for identical input upstream).
        return false;
    }
    let mut b = 0usize;
    let mut a = 0usize;
    while b < before.len() {
        // Advance `a` until we find before[b] or exhaust `after`.
        while a < after.len() && after[a] != before[b] {
            a += 1;
        }
        if a == after.len() {
            // before[b] not found in after — removal or modification.
            return false;
        }
        b += 1;
        a += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invasiveness::{classify, Invasiveness};

    #[test]
    fn identical_content_is_empty_diff() {
        let s = summarize_text_diff("hello\nworld\n", "hello\nworld\n");
        assert_eq!(s, DiffSummary::empty());
        assert_eq!(classify(&s), Invasiveness::Editorial); // safe fallback
    }

    #[test]
    fn trailing_whitespace_only_is_mechanical() {
        let s = summarize_text_diff("line one\nline two\n", "line one  \nline two\t\n");
        assert!(s.is_pure_metadata_normalization);
        assert_eq!(classify(&s), Invasiveness::Mechanical);
    }

    #[test]
    fn extra_blank_lines_collapse_to_mechanical() {
        let s = summarize_text_diff("para one\n\npara two\n", "para one\n\n\n\npara two\n\n\n");
        assert!(s.is_pure_metadata_normalization);
        assert_eq!(classify(&s), Invasiveness::Mechanical);
    }

    #[test]
    fn pure_line_insertion_at_end_classifies_editorial() {
        // Conservative walker: pure insertion sets `adds_new_blocks_only`
        // but NOT `additive_only_safe_kinds` (which requires AST kind
        // detection), so the classifier maps to Editorial.
        let s = summarize_text_diff(
            "existing one\nexisting two\n",
            "existing one\nexisting two\nbrand new line\n",
        );
        assert!(s.adds_new_blocks_only);
        assert!(!s.additive_only_safe_kinds);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn pure_line_insertion_in_middle_is_still_additive() {
        let s = summarize_text_diff(
            "line one\nline two\n",
            "line one\ninserted middle\nline two\n",
        );
        assert!(s.adds_new_blocks_only);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn in_place_modification_classifies_editorial() {
        let s = summarize_text_diff("the quick brown fox\n", "the lazy brown fox\n");
        assert!(s.modifies_existing_text_blocks);
        assert!(!s.adds_new_blocks_only);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn line_removal_classifies_editorial() {
        // Removal is the inverse of insertion; the walker falls
        // through to `modifies_existing_text_blocks=true` because
        // `is_pure_insertion` requires `after.len() > before.len()`.
        let s = summarize_text_diff("line one\nline two\nline three\n", "line one\nline three\n");
        assert!(!s.adds_new_blocks_only);
        assert!(s.modifies_existing_text_blocks);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn reordering_is_not_pure_insertion() {
        let s = summarize_text_diff("a\nb\nc\n", "c\na\nb\n");
        // `after` has same lines but reordered. Not pure insertion;
        // falls through to in-place modification.
        assert!(!s.adds_new_blocks_only);
        assert!(s.modifies_existing_text_blocks);
    }

    #[test]
    fn insertion_with_modification_is_not_pure_insertion() {
        // Real-world: agent added a section AND fixed a typo in an
        // existing line. The line modification disqualifies the
        // pure-insertion path.
        let s = summarize_text_diff(
            "the quick brown fox\nold ending\n",
            "the lazy brown fox\nold ending\nbrand new\n",
        );
        assert!(!s.adds_new_blocks_only);
        assert!(s.modifies_existing_text_blocks);
    }

    #[test]
    fn whitespace_normalization_takes_precedence_over_pure_insertion() {
        // If a diff is both whitespace-normalization AND pure
        // insertion, we want to report Mechanical (the cheaper class)
        // — both branches would be true, but the walker checks
        // is_pure_whitespace_normalization first and short-circuits.
        // Constructing such a case: "a\nb" + "a  \nb" — trailing ws on
        // "a" is whitespace-only AND every line of before appears in
        // after... actually `before[0] = "a"` but `after[0] = "a  "`,
        // and those aren't equal at the line level, so is_pure_insertion
        // returns false. The two cases are mostly disjoint at the
        // line level. Document the precedence anyway with an assertion.
        let s = summarize_text_diff("a\nb", "a  \nb");
        assert!(s.is_pure_metadata_normalization);
        assert!(!s.adds_new_blocks_only); // not pure insertion (line-level mismatch)
        assert_eq!(classify(&s), Invasiveness::Mechanical);
    }

    #[test]
    fn empty_to_nonempty_is_pure_insertion() {
        let s = summarize_text_diff("", "first line\n");
        assert!(s.adds_new_blocks_only);
        // Not safe-kinds → Editorial.
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn nonempty_to_empty_is_modification() {
        let s = summarize_text_diff("everything\n", "");
        assert!(!s.adds_new_blocks_only);
        assert!(s.modifies_existing_text_blocks);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }
}
