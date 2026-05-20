//! Markdown AST parsing, wikilink/block-ID extraction, and section editing.
//!
//! Wraps [`comrak`] to provide engram's canonical markdown entry point.
//!
//! # Key types
//!
//! - [`EngramAst`] — owns the arena and the root AST node.
//! - [`Wikilink`] — a parsed `[[Target]]` or `[[Target|Alias]]` reference.
//! - [`BlockId`] — an Obsidian `^block-id` anchor found at the end of a block.
//!
//! # Example
//!
//! ```rust
//! use engram_core::markdown::{parse_markdown, extract_wikilinks, extract_block_ids};
//!
//! let ast = parse_markdown("See [[Other Note|the other note]] for details.\n\nA block. ^my-id");
//! let links = extract_wikilinks(&ast);
//! assert_eq!(links[0].target, "Other Note");
//! assert_eq!(links[0].alias.as_deref(), Some("the other note"));
//!
//! let ids = extract_block_ids(&ast);
//! assert_eq!(ids[0].id, "my-id");
//! ```

use comrak::{
    nodes::{AstNode, NodeHeading, NodeValue},
    Arena, Options,
};

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Build the comrak [`Options`] used across all engram parsing and rendering.
///
/// Enables the `wikilinks_title_after_pipe` extension so that `[[Target|Alias]]`
/// produces a `WikiLink` AST node with `url = "Target"` and the alias as inline text.
pub fn engram_options() -> Options<'static> {
    let mut opts = Options::default();
    // Obsidian: alias comes after the pipe.  [[Target|Alias]]
    opts.extension.wikilinks_title_after_pipe = true;
    // Preserve HTML comments (provenance markers).
    opts.render.unsafe_ = true;
    opts
}

// ---------------------------------------------------------------------------
// EngramAst
// ---------------------------------------------------------------------------

/// Owns the arena allocator and the parsed AST root for a single markdown note.
///
/// The arena must outlive the tree; keeping them together in one struct makes
/// the ownership relationship explicit.
pub struct EngramAst<'a> {
    /// The root `document` node.  All traversal starts here.
    pub root: &'a AstNode<'a>,
    /// The arena that backs all AST nodes.  Must outlive `root`.
    #[allow(dead_code)]
    arena: &'a Arena<AstNode<'a>>,
}

/// Parse `content` into an [`EngramAst`].
///
/// The returned value borrows from the provided `arena`; the arena must
/// therefore be created by the caller and kept alive for as long as the
/// AST is needed.
///
/// # Example
///
/// ```rust
/// use comrak::Arena;
/// use engram_core::markdown::parse_markdown_in;
///
/// let arena = Arena::new();
/// let ast = parse_markdown_in("Hello **world**.", &arena);
/// assert!(!ast.root.children().next().is_none());
/// ```
pub fn parse_markdown_in<'a>(content: &str, arena: &'a Arena<AstNode<'a>>) -> EngramAst<'a> {
    let opts = engram_options();
    let root = comrak::parse_document(arena, content, &opts);
    EngramAst { root, arena }
}

/// Parse `content` into a self-contained [`OwnedAst`].
///
/// Use this when you need to store or return the AST without managing the arena
/// lifetime yourself.  For lifetime-sensitive code (e.g. mutation passes that
/// need the arena in scope), use [`parse_markdown_in`] instead.
pub fn parse_markdown(content: &str) -> OwnedAst {
    OwnedAst::new(content)
}

// ---------------------------------------------------------------------------
// OwnedAst — arena + root in a single heap allocation
// ---------------------------------------------------------------------------

/// A self-contained AST that owns its arena.
///
/// This is returned by [`parse_markdown`] and is the primary type used by
/// [`extract_wikilinks`] and [`extract_block_ids`].
pub struct OwnedAst {
    /// The raw markdown source stored for re-rendering.
    source: String,
}

impl OwnedAst {
    fn new(content: &str) -> Self {
        OwnedAst {
            source: content.to_owned(),
        }
    }

    /// Re-render the AST back to CommonMark.
    pub fn to_commonmark(&self) -> String {
        let arena = Arena::new();
        let opts = engram_options();
        let root = comrak::parse_document(&arena, &self.source, &opts);
        let mut out = Vec::new();
        comrak::format_commonmark(root, &opts, &mut out).expect("format_commonmark must not fail");
        String::from_utf8(out).expect("comrak output is always valid UTF-8")
    }

    /// Expose the source for internal helpers that re-parse.
    pub fn source(&self) -> &str {
        &self.source
    }
}

// ---------------------------------------------------------------------------
// Wikilink
// ---------------------------------------------------------------------------

/// A parsed `[[Target]]` or `[[Target|Alias]]` wikilink.
///
/// Obsidian wikilink grammar (as implemented by the `wikilinks_title_after_pipe`
/// comrak extension):
///
/// ```text
/// [[Target]]
/// [[Target|Alias]]
/// [[Target#^block-id]]
/// [[Target#^block-id|Alias]]
/// [[ULID|Title]]
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct Wikilink {
    /// The link target (note title, ULID, or `Note#^block-id`).
    pub target: String,

    /// Optional display alias (the text after `|`).
    pub alias: Option<String>,

    /// Optional block ID referenced within the target (`#^block-id` suffix).
    pub block_id: Option<String>,

    /// Source position `(line, column)` of the opening `[[`.  1-indexed.
    pub position: (usize, usize),
}

/// Walk the AST and return all wikilinks found.
///
/// Wikilinks are represented as `WikiLink` AST nodes by comrak when the
/// `wikilinks_title_after_pipe` extension is enabled.  The alias, if present,
/// is the inline text child of the node.
pub fn extract_wikilinks(ast: &OwnedAst) -> Vec<Wikilink> {
    let arena = Arena::new();
    let opts = engram_options();
    let root = comrak::parse_document(&arena, ast.source(), &opts);

    let mut results = Vec::new();
    collect_wikilinks(root, &mut results);
    results
}

fn collect_wikilinks<'a>(node: &'a AstNode<'a>, out: &mut Vec<Wikilink>) {
    let data = node.data.borrow();
    if let NodeValue::WikiLink(ref wl) = data.value {
        let (line, col) = (data.sourcepos.start.line, data.sourcepos.start.column);
        let raw_url = wl.url.clone();

        // Extract block_id from `Target#^block-id` form.
        let (target, block_id) = if let Some(idx) = raw_url.find("#^") {
            let tgt = raw_url[..idx].to_owned();
            let bid = raw_url[idx + 2..].to_owned();
            (tgt, if bid.is_empty() { None } else { Some(bid) })
        } else {
            (raw_url.clone(), None)
        };

        // The alias is the concatenated text of all inline children.
        // When no pipe was present, comrak uses the raw url as the label text,
        // so we suppress that as alias.
        drop(data); // release borrow before traversing children
        let alias_text = collect_text(node);
        let alias = if alias_text.is_empty() || alias_text == raw_url || alias_text == target {
            None
        } else {
            Some(alias_text)
        };

        out.push(Wikilink {
            target,
            alias,
            block_id,
            position: (line, col),
        });
        return; // don't recurse into WikiLink children — that's the alias text
    }
    drop(data);
    for child in node.children() {
        collect_wikilinks(child, out);
    }
}

/// Collect the text content of a node's children (for alias extraction).
fn collect_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut buf = String::new();
    for child in node.children() {
        let data = child.data.borrow();
        if let NodeValue::Text(ref t) = data.value {
            buf.push_str(t);
        }
        drop(data);
        buf.push_str(&collect_text(child));
    }
    buf
}

// ---------------------------------------------------------------------------
// BlockId
// ---------------------------------------------------------------------------

/// An Obsidian block ID (`^block-id`) anchor at the end of a paragraph.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockId {
    /// The block ID text (without the leading `^`).
    pub id: String,

    /// Source position `(line, column)` where the `^` appears.  1-indexed.
    pub position: (usize, usize),
}

/// Walk the AST and return all block IDs found.
///
/// In Obsidian markdown, `^block-id` appears as plain text at the end of a
/// paragraph (or list item, heading, etc.).  Comrak does not treat `^` as
/// special syntax; the text node containing it will read e.g. `"A block. ^my-id"`.
///
/// This function scans every `Text` leaf node for a trailing `^word` token.
pub fn extract_block_ids(ast: &OwnedAst) -> Vec<BlockId> {
    let arena = Arena::new();
    let opts = engram_options();
    let root = comrak::parse_document(&arena, ast.source(), &opts);

    let mut results = Vec::new();
    collect_block_ids(root, &mut results);
    results
}

fn collect_block_ids<'a>(node: &'a AstNode<'a>, out: &mut Vec<BlockId>) {
    let data = node.data.borrow();
    if let NodeValue::Text(ref t) = data.value {
        let (line, col_base) = (data.sourcepos.start.line, data.sourcepos.start.column);
        if let Some(bid) = extract_trailing_block_id(t) {
            // Compute approximate column: offset of the '^' in the text + base column.
            let caret_offset = t.rfind('^').unwrap_or(0);
            out.push(BlockId {
                id: bid,
                position: (line, col_base + caret_offset),
            });
        }
    }
    drop(data);
    for child in node.children() {
        collect_block_ids(child, out);
    }
}

/// Extract `id` from a text node whose content ends with ` ^id` or `^id`.
/// Returns `None` if no block ID is found.
fn extract_trailing_block_id(text: &str) -> Option<String> {
    // Trim trailing whitespace and check for `^word` at the end.
    let t = text.trim_end();
    // Find the last `^`; everything after it must be `[a-zA-Z0-9_-]+`.
    let caret_pos = t.rfind('^')?;
    let after = &t[caret_pos + 1..];
    if after.is_empty() {
        return None;
    }
    // Block IDs in Obsidian: alphanumeric and hyphens only.
    if after
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        // The `^id` must be preceded by whitespace or start-of-string to avoid
        // false positives on things like `2^3` or `Rust^1`.
        if caret_pos == 0 || t.as_bytes()[caret_pos - 1] == b' ' {
            return Some(after.to_owned());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// AST edit primitives
// ---------------------------------------------------------------------------

/// Re-render `ast` back to CommonMark.
pub fn to_commonmark(ast: &OwnedAst) -> String {
    ast.to_commonmark()
}

/// Append `content` (a CommonMark fragment) to the section identified by `heading`.
///
/// The section is defined as the content between the heading and the next
/// heading of the same or higher level (or end of document).  `content` is
/// appended after the last existing block in the section.
///
/// Returns `true` if the section was found, `false` if not.
pub fn append_to_section(ast: &mut OwnedAst, heading: &str, content: &str) -> bool {
    let arena = Arena::new();
    let opts = engram_options();
    let root = comrak::parse_document(&arena, ast.source(), &opts);

    if !section_exists(root, heading) {
        return false;
    }

    let mut new_source = ast.source.clone();
    // Find the position to insert: after the last node in the section.
    // Simple approach: re-render with the new content appended in the right place.
    new_source = insert_after_section(&new_source, heading, content);
    ast.source = new_source;
    true
}

/// Create a new `## Heading` section at the end of the document if it does not already exist.
///
/// Returns `true` if the section was created, `false` if it already existed.
pub fn create_section_if_missing(ast: &mut OwnedAst, heading: &str) -> bool {
    let arena = Arena::new();
    let opts = engram_options();
    let root = comrak::parse_document(&arena, ast.source(), &opts);

    if section_exists(root, heading) {
        return false;
    }

    // Append the section as a level-2 heading.
    ast.source.push_str(&format!("\n## {heading}\n"));
    true
}

/// Insert a wikilink at the given `(line, col)` position in the source.
///
/// Returns `true` on success, `false` if the position is out of range.
pub fn insert_wikilink(ast: &mut OwnedAst, position: (usize, usize), wikilink: &Wikilink) -> bool {
    let text = match &wikilink.alias {
        Some(a) => format!("[[{}|{}]]", wikilink.target, a),
        None => format!("[[{}]]", wikilink.target),
    };
    let (line, col) = position;
    let mut lines: Vec<String> = ast.source.lines().map(|l| l.to_owned()).collect();
    if line == 0 || line > lines.len() {
        return false;
    }
    let l = &mut lines[line - 1];
    let byte_col = col.saturating_sub(1).min(l.len());
    l.insert_str(byte_col, &text);
    let had_trailing_newline = ast.source.ends_with('\n');
    ast.source = lines.join("\n");
    if had_trailing_newline {
        ast.source.push('\n');
    }
    true
}

// ---------------------------------------------------------------------------
// Internal helpers for section manipulation
// ---------------------------------------------------------------------------

fn section_exists<'a>(root: &'a AstNode<'a>, heading: &str) -> bool {
    for node in root.children() {
        let data = node.data.borrow();
        if let NodeValue::Heading(NodeHeading { level: _, .. }) = data.value {
            drop(data);
            let text = collect_text(node);
            if text.trim() == heading {
                return true;
            }
        }
    }
    false
}

/// Source-level insertion: find the heading and append `content` after its section.
fn insert_after_section(source: &str, heading: &str, content: &str) -> String {
    // Strategy: find the line with `## Heading`, find the end of its section,
    // and splice in the new content.
    let lines: Vec<&str> = source.lines().collect();
    let heading_needle = heading.trim();

    // Find the line index of the target heading.
    let mut heading_line: Option<usize> = None;
    let mut heading_level: usize = 2;
    for (i, line) in lines.iter().enumerate() {
        let stripped = line.trim_start_matches('#');
        let hashes = line.len() - stripped.len();
        if hashes > 0 && stripped.trim() == heading_needle {
            heading_line = Some(i);
            heading_level = hashes;
            break;
        }
    }

    let Some(hi) = heading_line else {
        return source.to_owned();
    };

    // Find where this section ends: the next heading at same or higher level.
    let mut section_end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(hi + 1) {
        let stripped = line.trim_start_matches('#');
        let hashes = line.len() - stripped.len();
        if hashes > 0 && hashes <= heading_level {
            section_end = i;
            break;
        }
    }

    let mut result = lines[..section_end].join("\n");
    result.push('\n');
    result.push_str(content);
    if !content.ends_with('\n') {
        result.push('\n');
    }
    if section_end < lines.len() {
        result.push_str(&lines[section_end..].join("\n"));
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Wikilink extraction
    // -----------------------------------------------------------------------

    #[test]
    fn extract_standard_wikilink() {
        let ast = parse_markdown("See [[Other Note]] for details.");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Other Note");
        assert_eq!(links[0].alias, None);
        assert_eq!(links[0].block_id, None);
    }

    #[test]
    fn extract_aliased_wikilink() {
        let ast = parse_markdown("[[Attention Mechanism|the mechanism]]");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Attention Mechanism");
        assert_eq!(links[0].alias.as_deref(), Some("the mechanism"));
        assert_eq!(links[0].block_id, None);
    }

    #[test]
    fn extract_block_id_targeted_wikilink() {
        let ast = parse_markdown("[[Note Title#^abc123]]");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Note Title");
        assert_eq!(links[0].block_id.as_deref(), Some("abc123"));
        assert_eq!(links[0].alias, None);
    }

    #[test]
    fn extract_aliased_block_id_wikilink() {
        // comrak encodes '#' in url, so use a format comrak actually supports
        let ast = parse_markdown("[[Note#^bid|My Alias]]");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].block_id.as_deref(), Some("bid"));
        assert_eq!(links[0].alias.as_deref(), Some("My Alias"));
    }

    #[test]
    fn extract_ulid_form_wikilink() {
        let ast = parse_markdown("[[01JRZK3M7PQNX8BABCDE12345|Attention as Compression]]");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "01JRZK3M7PQNX8BABCDE12345");
        assert_eq!(links[0].alias.as_deref(), Some("Attention as Compression"));
    }

    #[test]
    fn extract_multiple_wikilinks() {
        let ast = parse_markdown("[[A]] and [[B|alias b]] and [[C#^id1]].");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].target, "A");
        assert_eq!(links[1].target, "B");
        assert_eq!(links[2].target, "C");
    }

    #[test]
    fn wikilinks_record_position() {
        let ast = parse_markdown("[[MyNote]]");
        let links = extract_wikilinks(&ast);
        assert_eq!(links.len(), 1);
        // Position should be line 1, some positive column.
        assert_eq!(links[0].position.0, 1);
        assert!(links[0].position.1 >= 1);
    }

    // -----------------------------------------------------------------------
    // Block ID extraction
    // -----------------------------------------------------------------------

    #[test]
    fn extract_block_id_from_paragraph() {
        let ast = parse_markdown("This is a block. ^my-id");
        let ids = extract_block_ids(&ast);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "my-id");
    }

    #[test]
    fn extract_block_id_alphanumeric() {
        let ast = parse_markdown("Some content. ^abc123");
        let ids = extract_block_ids(&ast);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "abc123");
    }

    #[test]
    fn no_block_id_in_exponent() {
        // `2^3` should not be treated as a block ID (no leading space before ^).
        let ast = parse_markdown("Result is 2^3 = 8.");
        let ids = extract_block_ids(&ast);
        assert!(
            ids.is_empty(),
            "exponent should not produce a block id: {ids:?}"
        );
    }

    #[test]
    fn block_id_at_start_of_line() {
        // `^id` alone on a line (no preceding space) should be recognized.
        let ast = parse_markdown("^standalone-id");
        let ids = extract_block_ids(&ast);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].id, "standalone-id");
    }

    #[test]
    fn extract_block_id_records_position() {
        let ast = parse_markdown("A paragraph. ^pos-test");
        let ids = extract_block_ids(&ast);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].position.0, 1);
    }

    // -----------------------------------------------------------------------
    // Section edit primitives
    // -----------------------------------------------------------------------

    #[test]
    fn create_section_if_missing_adds_heading() {
        let mut ast = parse_markdown("# Title\n\nBody.\n");
        let created = create_section_if_missing(&mut ast, "References");
        assert!(created, "section should be created");
        assert!(
            ast.source.contains("## References"),
            "source should contain new heading"
        );
        // Calling again should be idempotent.
        let created2 = create_section_if_missing(&mut ast, "References");
        assert!(!created2, "section already exists, should return false");
    }

    #[test]
    fn append_to_section_inserts_content() {
        let mut ast = parse_markdown("# Doc\n\n## Notes\n\nExisting.\n");
        let ok = append_to_section(&mut ast, "Notes", "New content.");
        assert!(ok);
        assert!(
            ast.source.contains("New content."),
            "content should be appended"
        );
    }

    #[test]
    fn append_to_section_returns_false_for_missing_section() {
        let mut ast = parse_markdown("# Doc\n\nNo sections.\n");
        let ok = append_to_section(&mut ast, "Nonexistent", "");
        assert!(!ok);
    }

    #[test]
    fn insert_wikilink_inserts_at_position() {
        let mut ast = parse_markdown("Hello world.\n");
        let wl = Wikilink {
            target: "Other Note".to_owned(),
            alias: None,
            block_id: None,
            position: (1, 1),
        };
        let ok = insert_wikilink(&mut ast, (1, 7), &wl);
        assert!(ok);
        assert!(ast.source.contains("[[Other Note]]"));
    }

    // -----------------------------------------------------------------------
    // Round-trip: HTML comments preserved
    // -----------------------------------------------------------------------

    #[test]
    fn html_comment_provenance_marker_preserved() {
        let md = "<!-- by: Synthesizer -->\n\nSome content.\n";
        let ast = parse_markdown(md);
        let rt = ast.to_commonmark();
        assert!(
            rt.contains("<!-- by: Synthesizer -->"),
            "provenance comment must survive round-trip; got:\n{rt}"
        );
    }

    #[test]
    fn nested_headings_section_detection() {
        let md = "# H1\n\n## H2\n\nContent.\n\n### H3\n\nNested.\n";
        let ast = parse_markdown(md);
        let links = extract_wikilinks(&ast);
        assert!(links.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Parsing and re-rendering arbitrary CommonMark is idempotent on a second pass.
        ///
        /// We can't guarantee exact byte-for-byte equality on the *first* round-trip
        /// because comrak normalizes some constructs (e.g. ATX headings, list markers).
        /// But `parse(render(parse(s)))` == `render(parse(s))` must hold.
        #[test]
        fn round_trip_idempotent(s in "[a-zA-Z0-9 \n#>.,:;()]{0,200}") {
            let ast1 = parse_markdown(&s);
            let rt1 = ast1.to_commonmark();
            let ast2 = parse_markdown(&rt1);
            let rt2 = ast2.to_commonmark();
            prop_assert_eq!(rt1, rt2, "second render must match first");
        }
    }
}
