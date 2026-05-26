//! Markdown-AST [`DiffSummary`] walker — the AST-aware counterpart to
//! the conservative text-level walker in [`crate::diff_walker`].
//!
//! The text walker handles whitespace normalization and pure line
//! insertion, but leaves three classifier inputs permanently unset
//! because they require structure-aware analysis:
//!
//! - **`modifies_critical_frontmatter`** — needs YAML parsing
//! - **`removes_links`** — needs wikilink extraction
//! - **`additive_only_safe_kinds`** — needs block-kind detection
//!
//! Together these are the three signals that gate `Structural` and
//! `Additive` verdicts in [`crate::invasiveness::classify`]. Without
//! the AST walker, agents like Linker and Cartographer can never
//! reach `Additive` even for a clean wikilink-add, and `Structural`
//! identity-frontmatter mutations are invisible — both modes the
//! invasiveness classifier explicitly models.
//!
//! # Behavioral contract
//!
//! `summarize_markdown_diff` is a **drop-in replacement** for
//! `summarize_text_diff`. It preserves every signal the text walker
//! sets (whitespace normalization, pure additive at the line level)
//! and ORs the three AST-derived signals on top. The text walker's
//! conservative bias is preserved: a diff that looks additive at the
//! line level but the AST can't confirm safe-kinds for stays
//! `Editorial` (council review), never silently auto-lands.
//!
//! # Identity-critical frontmatter set
//!
//! The keys `id`, `type`, `title` form the identity triad per
//! `docs/design/06-storage-format-and-sidecar.md`. Any change to
//! these — including added or removed keys — counts as
//! `modifies_critical_frontmatter` and forces `Structural`. The
//! walker treats malformed YAML on either side as "no critical
//! frontmatter present" (parse failure ≠ identity change), but if
//! one side parses and the other doesn't, the asymmetric absence of
//! the critical keys counts as a change.
//!
//! # Safe additive block kinds
//!
//! Per the spec table, only three block kinds may auto-land as
//! `Additive`:
//!
//! - **`section_heading`** — comrak `NodeValue::Heading`
//! - **`html_comment`** — comrak `NodeValue::HtmlBlock` whose literal
//!   begins with `<!--`
//! - **`wikilink`** — a paragraph whose every inline child is a
//!   `WikiLink` (possibly separated by whitespace `Text`); this is
//!   the "Linker added a `[[backlink]]` in its own paragraph" case
//!
//! A paragraph containing prose plus a wikilink is *not* a safe
//! wikilink block — the prose is unconstrained text.

use std::collections::BTreeSet;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{parse_document, Arena, Options};
use serde_yaml::Value as YamlValue;

use crate::diff_walker::summarize_text_diff;
use crate::invasiveness::DiffSummary;

/// AST-aware summary of the diff between `before` and `after`.
///
/// Returns the text walker's output augmented with three AST-derived
/// signals. Identical input returns the empty summary (the text
/// walker's behavior is preserved verbatim for that case).
pub fn summarize_markdown_diff(before: &str, after: &str) -> DiffSummary {
    let mut summary = summarize_text_diff(before, after);
    if before == after {
        return summary;
    }

    if critical_frontmatter_differs(before, after) {
        summary.modifies_critical_frontmatter = true;
    }

    let before_links = extract_wikilink_targets(before);
    let after_links = extract_wikilink_targets(after);
    // Any wikilink target present in `before` but missing from
    // `after` counts as a removal — even if a different target was
    // added in its place. `removes_links` is about loss, not net.
    if before_links.iter().any(|t| !after_links.contains(t)) {
        summary.removes_links = true;
    }

    // Only refine the additive-kind signal when the text walker
    // already determined the diff is purely additive at the block
    // level. If the text walker disagrees (e.g. a paragraph was also
    // modified in place), the AC explicitly says the diff stays
    // `Editorial` and we must not promote it to `Additive` here.
    if summary.adds_new_blocks_only && added_blocks_all_safe(before, after) {
        summary.additive_only_safe_kinds = true;
    }

    summary
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// Split off the leading YAML frontmatter block (between the first
/// pair of `---` delimiter lines). Returns `(yaml_body, rest)` or
/// `(None, original)` when no frontmatter is present.
fn split_frontmatter(s: &str) -> (Option<&str>, &str) {
    // Tolerate a UTF-8 BOM at the very start of the file.
    let trimmed = s.strip_prefix('\u{feff}').unwrap_or(s);
    if !trimmed.starts_with("---") {
        return (None, s);
    }
    // Skip the opening `---` and its newline.
    let after_open = match trimmed.find('\n') {
        Some(i) => &trimmed[i + 1..],
        None => return (None, s),
    };
    // Find the closing `---` line.
    for (idx, line) in after_open.split_inclusive('\n').scan(0usize, |acc, l| {
        let start = *acc;
        *acc += l.len();
        Some((start, l))
    }) {
        let line_trimmed = line.trim_end_matches('\n');
        if line_trimmed == "---" || line_trimmed == "..." {
            let yaml = &after_open[..idx];
            let after_close = &after_open[idx + line.len()..];
            return (Some(yaml), after_close);
        }
    }
    (None, s)
}

/// True iff any of `id` / `type` / `title` differs between the two
/// frontmatter blocks (including added or removed keys).
fn critical_frontmatter_differs(before: &str, after: &str) -> bool {
    let b_yaml = split_frontmatter(before).0;
    let a_yaml = split_frontmatter(after).0;

    // If neither side has frontmatter, the critical fields are
    // identically absent — no change.
    if b_yaml.is_none() && a_yaml.is_none() {
        return false;
    }

    let parse = |y: Option<&str>| -> Option<YamlValue> {
        y.and_then(|s| serde_yaml::from_str::<YamlValue>(s).ok())
    };
    let b = parse(b_yaml);
    let a = parse(a_yaml);

    for key in ["id", "type", "title"] {
        let bv = b.as_ref().and_then(|v| v.get(key));
        let av = a.as_ref().and_then(|v| v.get(key));
        if bv != av {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Wikilink extraction
// ---------------------------------------------------------------------------

fn engram_opts() -> Options<'static> {
    let mut o = Options::default();
    o.extension.wikilinks_title_after_pipe = true;
    // Preserve HTML comments so `<!-- ... -->` blocks classify as
    // HtmlBlock rather than being elided.
    o.render.unsafe_ = true;
    o
}

/// Set of wikilink *targets* (the part before `|` and before `#^`).
/// Aliases and block-ids are irrelevant to "did this link exist".
fn extract_wikilink_targets(s: &str) -> BTreeSet<String> {
    let arena = Arena::new();
    let opts = engram_opts();
    let root = parse_document(&arena, s, &opts);
    let mut out = BTreeSet::new();
    collect_wikilinks(root, &mut out);
    out
}

fn collect_wikilinks<'a>(node: &'a AstNode<'a>, out: &mut BTreeSet<String>) {
    {
        let data = node.data.borrow();
        if let NodeValue::WikiLink(ref wl) = data.value {
            let target = match wl.url.find("#^") {
                Some(i) => &wl.url[..i],
                None => &wl.url,
            };
            out.insert(target.to_string());
            return; // don't recurse into the alias text
        }
    }
    for child in node.children() {
        collect_wikilinks(child, out);
    }
}

// ---------------------------------------------------------------------------
// Additive-kind safety
// ---------------------------------------------------------------------------

/// True iff every block in `after` that wasn't already in `before`
/// is one of the safe additive kinds (heading, html comment,
/// wikilink-only paragraph). Returns false if `after` adds no new
/// blocks at all — that case is not "additive of safe kinds", it's
/// "no additive content to vouch for".
fn added_blocks_all_safe(before: &str, after: &str) -> bool {
    let before_sigs = block_signatures(before);
    let after_blocks = block_kinds_with_signatures(after);

    let mut new_blocks = Vec::new();
    for (sig, kind) in &after_blocks {
        if !before_sigs.contains(sig) {
            new_blocks.push(kind);
        }
    }
    if new_blocks.is_empty() {
        return false;
    }
    new_blocks
        .iter()
        .all(|k| matches!(k.as_str(), "section_heading" | "html_comment" | "wikilink"))
}

/// Just the textual signatures of each top-level block (for "is this
/// block present in before" comparison).
fn block_signatures(s: &str) -> BTreeSet<String> {
    block_kinds_with_signatures(s)
        .into_iter()
        .map(|(sig, _)| sig)
        .collect()
}

/// `(signature, kind)` for each top-level block in `s`. Signature is
/// the concatenated text content; kind is one of the strings the
/// safe-kind check matches against.
fn block_kinds_with_signatures(s: &str) -> Vec<(String, String)> {
    let arena = Arena::new();
    let opts = engram_opts();
    let root = parse_document(&arena, s, &opts);
    let mut out = Vec::new();
    for child in root.children() {
        let kind = classify_block(child);
        let sig = node_text(child);
        out.push((sig, kind));
    }
    out
}

/// Classify a top-level block. Paragraphs are split into `wikilink`
/// (every inline child is a `WikiLink` modulo whitespace) and
/// `paragraph` (anything else). HTML blocks whose literal begins
/// with `<!--` are `html_comment`; other HTML blocks are `html`
/// (unsafe).
fn classify_block<'a>(node: &'a AstNode<'a>) -> String {
    let data = node.data.borrow();
    match &data.value {
        NodeValue::Heading(_) => "section_heading".into(),
        NodeValue::HtmlBlock(h) => {
            if h.literal.trim_start().starts_with("<!--") {
                "html_comment".into()
            } else {
                "html".into()
            }
        }
        NodeValue::Paragraph => {
            drop(data);
            if paragraph_is_wikilink_only(node) {
                "wikilink".into()
            } else {
                "paragraph".into()
            }
        }
        NodeValue::List(_) => "list".into(),
        NodeValue::CodeBlock(_) => "code_block".into(),
        NodeValue::BlockQuote => "block_quote".into(),
        NodeValue::ThematicBreak => "thematic_break".into(),
        NodeValue::Table(_) => "table".into(),
        _ => "other".into(),
    }
}

/// True iff every inline child of the paragraph is a `WikiLink` or
/// whitespace-only `Text`. A trailing newline or a separator space
/// between two wikilinks is allowed; any prose disqualifies.
fn paragraph_is_wikilink_only<'a>(node: &'a AstNode<'a>) -> bool {
    let mut saw_wikilink = false;
    for child in node.children() {
        let data = child.data.borrow();
        match &data.value {
            NodeValue::WikiLink(_) => saw_wikilink = true,
            NodeValue::Text(t) if t.trim().is_empty() => {}
            NodeValue::SoftBreak | NodeValue::LineBreak => {}
            _ => return false,
        }
    }
    saw_wikilink
}

/// Concatenated text content of a node — used as a per-block
/// signature for "is this block already present" comparisons.
/// Includes URL strings for wikilinks so two identical-looking
/// paragraphs with different link targets don't collide.
fn node_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut buf = String::new();
    collect_text(node, &mut buf);
    buf
}

fn collect_text<'a>(node: &'a AstNode<'a>, buf: &mut String) {
    {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Text(t) => buf.push_str(t),
            NodeValue::Code(c) => buf.push_str(&c.literal),
            NodeValue::CodeBlock(c) => buf.push_str(&c.literal),
            NodeValue::HtmlBlock(h) => buf.push_str(&h.literal),
            NodeValue::HtmlInline(h) => buf.push_str(h),
            NodeValue::WikiLink(w) => {
                buf.push_str("[[");
                buf.push_str(&w.url);
                buf.push_str("]]");
            }
            _ => {}
        }
    }
    for c in node.children() {
        collect_text(c, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invasiveness::{classify, Invasiveness};

    // ----- spec table: each branch reaches the expected verdict -----

    #[test]
    fn identical_content_is_empty_diff() {
        let s = summarize_markdown_diff("hello\n", "hello\n");
        assert_eq!(s, DiffSummary::empty());
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn link_removal_sets_removes_links_and_classifies_editorial() {
        let before = "see [[old-note]] for context.\n";
        let after = "see this for context.\n";
        let s = summarize_markdown_diff(before, after);
        assert!(
            s.removes_links,
            "link present in before but absent from after must trip removes_links"
        );
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn link_swap_still_counts_as_removal() {
        // Even though after has the same link count, the specific
        // target `old-note` was removed.
        let before = "see [[old-note]].\n";
        let after = "see [[new-note]].\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.removes_links);
    }

    #[test]
    fn link_addition_does_not_set_removes_links() {
        let before = "see this for context.\n";
        let after = "see [[new-note]] for context.\n";
        let s = summarize_markdown_diff(before, after);
        assert!(!s.removes_links);
    }

    #[test]
    fn critical_frontmatter_id_change_is_structural() {
        let before = "---\nid: 01HXAAA\ntitle: A\n---\n\nbody\n";
        let after = "---\nid: 01HXBBB\ntitle: A\n---\n\nbody\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.modifies_critical_frontmatter);
        assert_eq!(classify(&s), Invasiveness::Structural);
    }

    #[test]
    fn critical_frontmatter_title_change_is_structural() {
        let before = "---\nid: 01HX\ntitle: First\n---\n\nbody\n";
        let after = "---\nid: 01HX\ntitle: Second\n---\n\nbody\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.modifies_critical_frontmatter);
        assert_eq!(classify(&s), Invasiveness::Structural);
    }

    #[test]
    fn critical_frontmatter_type_change_is_structural() {
        let before = "---\nid: 01HX\ntype: fleeting\ntitle: A\n---\n\nbody\n";
        let after = "---\nid: 01HX\ntype: evergreen\ntitle: A\n---\n\nbody\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.modifies_critical_frontmatter);
        assert_eq!(classify(&s), Invasiveness::Structural);
    }

    #[test]
    fn critical_frontmatter_added_key_is_structural() {
        let before = "---\ntitle: A\n---\n\nbody\n";
        let after = "---\nid: 01HX\ntitle: A\n---\n\nbody\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.modifies_critical_frontmatter);
    }

    #[test]
    fn non_critical_frontmatter_change_is_not_structural() {
        // `tags` / `status` aren't in the identity triad.
        let before = "---\nid: 01HX\ntitle: A\ntags: []\n---\n\nbody\n";
        let after = "---\nid: 01HX\ntitle: A\ntags: [one]\n---\n\nbody\n";
        let s = summarize_markdown_diff(before, after);
        assert!(!s.modifies_critical_frontmatter);
    }

    #[test]
    fn pure_additive_wikilink_block_is_additive() {
        // Linker pattern: append a paragraph that's just `[[link]]`.
        let before = "# Note\n\nbody paragraph.\n";
        let after = "# Note\n\nbody paragraph.\n\n[[related-note]]\n";
        let s = summarize_markdown_diff(before, after);
        assert!(
            s.adds_new_blocks_only,
            "text walker must agree this is line-additive"
        );
        assert!(
            s.additive_only_safe_kinds,
            "AST walker must recognize the new paragraph as a wikilink-only block"
        );
        assert_eq!(classify(&s), Invasiveness::Additive);
    }

    #[test]
    fn pure_additive_section_heading_is_additive() {
        let before = "# Existing\n\nbody.\n";
        let after = "# Existing\n\nbody.\n\n## New Section\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.adds_new_blocks_only);
        assert!(s.additive_only_safe_kinds);
        assert_eq!(classify(&s), Invasiveness::Additive);
    }

    #[test]
    fn pure_additive_html_comment_is_additive() {
        let before = "body.\n";
        let after = "body.\n\n<!-- provenance: agent=linker -->\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.adds_new_blocks_only);
        assert!(s.additive_only_safe_kinds);
        assert_eq!(classify(&s), Invasiveness::Additive);
    }

    #[test]
    fn pure_additive_code_block_stays_editorial() {
        // Code blocks aren't a safe-additive kind — even when purely
        // additive at the line level they classify Editorial.
        let before = "intro.\n";
        let after = "intro.\n\n```\nfn main() {}\n```\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.adds_new_blocks_only);
        assert!(!s.additive_only_safe_kinds);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    #[test]
    fn realistic_linker_inline_wikilink_stays_editorial() {
        // AC test case: agent inserts `[[new]]` *inside* an existing
        // paragraph. The paragraph itself is modified, so the text
        // walker correctly says `modifies_existing_text_blocks` —
        // and the AST walker must not promote it to Additive.
        let before = "this is an existing paragraph.\n";
        let after = "this is an existing paragraph mentioning [[new]].\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.modifies_existing_text_blocks);
        assert!(!s.adds_new_blocks_only);
        assert!(!s.additive_only_safe_kinds);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    // ----- preserves text-walker detections -----

    #[test]
    fn whitespace_normalization_still_mechanical() {
        let s = summarize_markdown_diff("a\nb\n", "a  \nb\t\n");
        assert!(s.is_pure_metadata_normalization);
        assert_eq!(classify(&s), Invasiveness::Mechanical);
    }

    #[test]
    fn pure_text_insertion_without_safe_kind_evidence_stays_editorial() {
        // Pure additive, but the new block is a plain paragraph —
        // not safe. Stays Editorial (text walker's behavior preserved).
        let before = "first.\n";
        let after = "first.\n\nsecond ordinary paragraph.\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.adds_new_blocks_only);
        assert!(!s.additive_only_safe_kinds);
        assert_eq!(classify(&s), Invasiveness::Editorial);
    }

    // ----- edge cases on frontmatter parsing -----

    #[test]
    fn malformed_frontmatter_does_not_panic() {
        let before = "---\nid: : invalid :: yaml\n---\n\nbody\n";
        let after = "---\nid: : also invalid\n---\n\nbody\n";
        // Should not panic; flag is conservatively false when both
        // sides fail to parse (no identity-claim to compare).
        let s = summarize_markdown_diff(before, after);
        assert!(!s.modifies_critical_frontmatter);
    }

    #[test]
    fn no_frontmatter_on_either_side_is_no_change() {
        let s = summarize_markdown_diff("plain body\n", "plain body modified\n");
        assert!(!s.modifies_critical_frontmatter);
    }

    #[test]
    fn frontmatter_added_when_absent_before_with_id_is_structural() {
        let before = "plain body\n";
        let after = "---\nid: 01HX\n---\n\nplain body\n";
        let s = summarize_markdown_diff(before, after);
        assert!(s.modifies_critical_frontmatter);
    }
}
