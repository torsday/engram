//! YAML frontmatter parsing and serialization (Obsidian-compatible).
//!
//! Parses the `---\n...\n---` block at the top of a markdown note into a
//! typed [`Frontmatter`] struct, and serializes it back deterministically.
//!
//! # Example
//!
//! ```rust
//! use engram_core::frontmatter::{parse_frontmatter, serialize_frontmatter, Frontmatter, NoteType};
//!
//! let md = "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: My Note\ntype: evergreen\n---\n\nBody.";
//! let fm = parse_frontmatter(md).unwrap();
//! assert_eq!(fm.note_type, NoteType::Evergreen);
//! let out = serialize_frontmatter(&fm);
//! assert!(out.starts_with("---\n"));
//! ```

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from frontmatter parsing or serialization.
#[derive(Debug, Error, PartialEq)]
pub enum FrontmatterError {
    /// No YAML frontmatter block found (missing leading `---`).
    #[error("no frontmatter block found — note must start with '---'")]
    NotFound,

    /// The frontmatter block is not properly closed with a second `---`.
    #[error("frontmatter block is not closed — missing closing '---'")]
    Unclosed,

    /// YAML parsing failed; the inner string provides field-level context.
    #[error("YAML parse error: {0}")]
    Yaml(String),

    /// A required field is absent.
    #[error("missing required field '{field}'")]
    MissingField { field: &'static str },

    /// A field had an unexpected type or value.
    #[error("invalid value for field '{field}': {reason}")]
    InvalidField { field: &'static str, reason: String },

    /// YAML serialization failed.
    #[error("serialization error: {0}")]
    Serialize(String),
}

// ---------------------------------------------------------------------------
// NoteType
// ---------------------------------------------------------------------------

/// The `type:` field of a note, as specified in `docs/design/06-note-conventions.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoteType {
    /// Quick captures, voice memos, share-sheet drops.
    Fleeting,
    /// One per ingested source.
    Literature,
    /// Curated, atomic concept notes.
    Evergreen,
    /// Maps of content / index.
    Moc,
    /// Corpus-digestion preserved (read-only, inert).
    Archive,
    /// Personal/dated entries.
    Journal,
    /// Sustained counter-argument to an evergreen.
    Heretical,
    /// Council transcript (in `.engram/`).
    Deliberation,
}

// ---------------------------------------------------------------------------
// NoteStatus
// ---------------------------------------------------------------------------

/// The `status:` field of a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoteStatus {
    Draft,
    CandidateEvergreen,
    Evergreen,
    NeedsReview,
    Contested,
}

// ---------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------

/// Strongly typed representation of a note's YAML frontmatter.
///
/// All fields not listed here are rejected (`deny_unknown_fields`), making typos
/// loudly visible rather than silently dropped.
///
/// Required fields: `id`, `title`, `note_type`.
/// All other fields are optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontmatter {
    /// ULID. Canonical note identity. Never changes.
    pub id: String,

    /// Human-readable title.
    pub title: String,

    /// Note classification.
    #[serde(rename = "type")]
    pub note_type: NoteType,

    /// Lifecycle status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<NoteStatus>,

    /// ISO date of creation (`YYYY-MM-DD`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,

    /// Slash-namespaced tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Alternative titles for wikilink resolution; order is preserved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    // -----------------------------------------------------------------------
    // Type-specific optional fields
    // -----------------------------------------------------------------------
    /// (`literature`) Source URL for human navigation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,

    /// (`literature`) Author list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,

    /// (`literature`) Publication year.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<u32>,

    /// (`heretical`) ULID of the note this contradicts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenges: Option<String>,

    /// (`archive`) Short slug for the source corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_corpus: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract and parse the YAML frontmatter from `content`.
///
/// `content` must begin with `---\n` (or `---\r\n`). Everything between the
/// opening and closing `---` is treated as YAML.
///
/// Returns a typed [`Frontmatter`] on success, or a [`FrontmatterError`]
/// with field-level context on failure.
pub fn parse_frontmatter(content: &str) -> Result<Frontmatter, FrontmatterError> {
    let yaml = extract_yaml_block(content)?;
    serde_yaml::from_str(yaml).map_err(|e| {
        // serde_yaml error messages include the field path on type mismatches.
        FrontmatterError::Yaml(e.to_string())
    })
}

/// Serialize `fm` into a `---\n…\n---\n` block.
///
/// The output always ends with a newline after the closing `---`.
pub fn serialize_frontmatter(fm: &Frontmatter) -> String {
    let yaml = serde_yaml::to_string(fm)
        .unwrap_or_else(|e| panic!("frontmatter serialization must not fail: {e}"));
    // serde_yaml::to_string already adds a trailing newline.
    // Strip the leading "---\n" that serde_yaml 0.9 prepends — we add our own delimiters.
    let body = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    format!("---\n{}---\n", body)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return the raw YAML text between the first pair of `---` delimiters.
fn extract_yaml_block(content: &str) -> Result<&str, FrontmatterError> {
    // Must start with `---` (optionally followed by \r\n or \n).
    let rest = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))
        .ok_or(FrontmatterError::NotFound)?;

    // Find the closing `---`.
    let close = rest
        .find("\n---\r\n")
        .map(|i| (i + 1, i + 1 + 5)) // position of `---\r\n` start, end
        .or_else(|| rest.find("\n---\n").map(|i| (i + 1, i + 1 + 4)))
        .or_else(|| {
            // Edge case: closing `---` is at the very end of the string with no trailing newline.
            if rest.ends_with("\n---") {
                let i = rest.len() - 3;
                Some((i, rest.len()))
            } else {
                None
            }
        })
        .ok_or(FrontmatterError::Unclosed)?;

    Ok(&rest[..close.0])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_frontmatter — happy path
    // -----------------------------------------------------------------------

    #[test]
    fn minimal_valid_frontmatter() {
        let md = "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: Hello\ntype: fleeting\n---\n\nBody.";
        let fm = parse_frontmatter(md).expect("should parse");
        assert_eq!(fm.id, "01JRZK3M7PQNX8BABCDE12345");
        assert_eq!(fm.title, "Hello");
        assert_eq!(fm.note_type, NoteType::Fleeting);
        assert!(fm.tags.is_empty());
        assert!(fm.aliases.is_empty());
        assert!(fm.status.is_none());
    }

    #[test]
    fn all_common_optional_fields() {
        let md = indoc::indoc! {"
            ---
            id: 01JRZK3M7PQNX8BABCDE99999
            title: Deep Work
            type: evergreen
            status: draft
            created: 2026-01-01
            tags:
              - topic/focus
              - topic/productivity
            aliases:
              - Deep Focus
            ---
        "};
        let fm = parse_frontmatter(md).expect("should parse");
        assert_eq!(fm.note_type, NoteType::Evergreen);
        assert_eq!(fm.status, Some(NoteStatus::Draft));
        assert_eq!(fm.created.as_deref(), Some("2026-01-01"));
        assert_eq!(fm.tags, vec!["topic/focus", "topic/productivity"]);
        assert_eq!(fm.aliases, vec!["Deep Focus"]);
    }

    #[test]
    fn literature_type_specific_fields() {
        let md = indoc::indoc! {"
            ---
            id: 01JRZK3M7PQNX8BABCDE00001
            title: Attention Is All You Need
            type: literature
            source_url: https://arxiv.org/abs/1706.03762
            authors:
              - Vaswani et al.
            published: 2017
            ---
        "};
        let fm = parse_frontmatter(md).expect("should parse");
        assert_eq!(fm.note_type, NoteType::Literature);
        assert_eq!(
            fm.source_url.as_deref(),
            Some("https://arxiv.org/abs/1706.03762")
        );
        assert_eq!(fm.authors, vec!["Vaswani et al."]);
        assert_eq!(fm.published, Some(2017));
    }

    #[test]
    fn heretical_type_specific_fields() {
        let md = indoc::indoc! {"
            ---
            id: 01JRZK3M7PQNX8BABCDE00002
            title: Against Slow Thinking
            type: heretical
            challenges: 01JRZK3M7PQNX8BABCDE11111
            ---
        "};
        let fm = parse_frontmatter(md).expect("should parse");
        assert_eq!(fm.note_type, NoteType::Heretical);
        assert_eq!(fm.challenges.as_deref(), Some("01JRZK3M7PQNX8BABCDE11111"));
    }

    #[test]
    fn archive_type_specific_fields() {
        let md = indoc::indoc! {"
            ---
            id: 01JRZK3M7PQNX8BABCDE00003
            title: Old Journal Import
            type: archive
            source_corpus: notes-2022-03
            ---
        "};
        let fm = parse_frontmatter(md).expect("should parse");
        assert_eq!(fm.note_type, NoteType::Archive);
        assert_eq!(fm.source_corpus.as_deref(), Some("notes-2022-03"));
    }

    #[test]
    fn all_note_types_parse() {
        for (type_str, expected) in [
            ("fleeting", NoteType::Fleeting),
            ("literature", NoteType::Literature),
            ("evergreen", NoteType::Evergreen),
            ("moc", NoteType::Moc),
            ("archive", NoteType::Archive),
            ("journal", NoteType::Journal),
            ("heretical", NoteType::Heretical),
            ("deliberation", NoteType::Deliberation),
        ] {
            let md =
                format!("---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: T\ntype: {type_str}\n---\n");
            let fm = parse_frontmatter(&md).unwrap_or_else(|e| {
                panic!("type '{type_str}' should parse: {e}");
            });
            assert_eq!(fm.note_type, expected, "type string: {type_str}");
        }
    }

    // -----------------------------------------------------------------------
    // parse_frontmatter — error cases
    // -----------------------------------------------------------------------

    #[test]
    fn missing_frontmatter_block() {
        let err = parse_frontmatter("# Just a heading\n\nNo frontmatter.").unwrap_err();
        assert_eq!(err, FrontmatterError::NotFound);
    }

    #[test]
    fn unclosed_frontmatter_block() {
        let err = parse_frontmatter("---\nid: abc\ntitle: T\ntype: fleeting\n").unwrap_err();
        assert_eq!(err, FrontmatterError::Unclosed);
    }

    #[test]
    fn missing_required_field_id() {
        let md = "---\ntitle: Hello\ntype: fleeting\n---\n";
        let err = parse_frontmatter(md).unwrap_err();
        assert!(
            matches!(err, FrontmatterError::Yaml(_)),
            "expected Yaml error for missing id, got: {err:?}"
        );
    }

    #[test]
    fn missing_required_field_title() {
        let md = "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntype: fleeting\n---\n";
        let err = parse_frontmatter(md).unwrap_err();
        assert!(
            matches!(err, FrontmatterError::Yaml(_)),
            "expected Yaml error for missing title, got: {err:?}"
        );
    }

    #[test]
    fn invalid_enum_variant() {
        let md = "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: T\ntype: INVALID_TYPE\n---\n";
        let err = parse_frontmatter(md).unwrap_err();
        assert!(
            matches!(err, FrontmatterError::Yaml(_)),
            "expected Yaml error for invalid enum, got: {err:?}"
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let md =
            "---\nid: 01JRZK3M7PQNX8BABCDE12345\ntitle: T\ntype: fleeting\nXXX_unknown: val\n---\n";
        let err = parse_frontmatter(md).unwrap_err();
        assert!(
            matches!(err, FrontmatterError::Yaml(_)),
            "expected Yaml error for unknown field, got: {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // serialize_frontmatter
    // -----------------------------------------------------------------------

    #[test]
    fn serialize_produces_delimited_block() {
        let fm = minimal_fm();
        let out = serialize_frontmatter(&fm);
        assert!(out.starts_with("---\n"), "must start with ---\\n");
        assert!(out.ends_with("---\n"), "must end with ---\\n");
    }

    #[test]
    fn serialize_omits_none_and_empty_vec_fields() {
        let fm = minimal_fm();
        let out = serialize_frontmatter(&fm);
        assert!(!out.contains("status:"), "status must be omitted when None");
        assert!(!out.contains("tags:"), "tags must be omitted when empty");
        assert!(
            !out.contains("aliases:"),
            "aliases must be omitted when empty"
        );
        assert!(
            !out.contains("source_url:"),
            "source_url must be omitted when None"
        );
    }

    // -----------------------------------------------------------------------
    // Round-trip: parse(serialize(fm)) == fm
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_minimal() {
        let fm = minimal_fm();
        let out = serialize_frontmatter(&fm);
        let fm2 = parse_frontmatter(&out).expect("round-trip parse failed");
        assert_eq!(fm, fm2);
    }

    #[test]
    fn round_trip_with_all_fields() {
        let fm = Frontmatter {
            id: "01JRZK3M7PQNX8BABCDE12345".into(),
            title: "Round-trip full".into(),
            note_type: NoteType::Literature,
            status: Some(NoteStatus::NeedsReview),
            created: Some("2026-05-20".into()),
            tags: vec!["topic/rust".into(), "topic/testing".into()],
            aliases: vec!["RT Full".into()],
            source_url: Some("https://example.com".into()),
            authors: vec!["Alice".into()],
            published: Some(2024),
            challenges: None,
            source_corpus: None,
        };
        let out = serialize_frontmatter(&fm);
        let fm2 = parse_frontmatter(&out).expect("round-trip parse failed");
        assert_eq!(fm, fm2);
    }

    // -----------------------------------------------------------------------
    // Helper
    // -----------------------------------------------------------------------

    fn minimal_fm() -> Frontmatter {
        Frontmatter {
            id: "01JRZK3M7PQNX8BABCDE12345".into(),
            title: "Minimal".into(),
            note_type: NoteType::Fleeting,
            status: None,
            created: None,
            tags: vec![],
            aliases: vec![],
            source_url: None,
            authors: vec![],
            published: None,
            challenges: None,
            source_corpus: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Property-based tests (proptest)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_note_type() -> impl Strategy<Value = NoteType> {
        prop_oneof![
            Just(NoteType::Fleeting),
            Just(NoteType::Literature),
            Just(NoteType::Evergreen),
            Just(NoteType::Moc),
            Just(NoteType::Archive),
            Just(NoteType::Journal),
            Just(NoteType::Heretical),
            Just(NoteType::Deliberation),
        ]
    }

    fn arb_note_status() -> impl Strategy<Value = NoteStatus> {
        prop_oneof![
            Just(NoteStatus::Draft),
            Just(NoteStatus::CandidateEvergreen),
            Just(NoteStatus::Evergreen),
            Just(NoteStatus::NeedsReview),
            Just(NoteStatus::Contested),
        ]
    }

    /// Non-empty strings safe for YAML scalar values (no leading `{`, `[`, etc.).
    fn arb_yaml_safe_string() -> impl Strategy<Value = String> {
        // ASCII alphanumeric + spaces + common punctuation; exclude YAML special chars
        "[a-zA-Z0-9 _/\\-\\.]{1,40}"
            .prop_map(|s| s.trim().to_string())
            .prop_filter("non-empty after trim", |s| !s.is_empty())
    }

    fn arb_frontmatter() -> impl Strategy<Value = Frontmatter> {
        (
            arb_note_type(),
            arb_yaml_safe_string(),
            arb_yaml_safe_string(),
            proptest::option::of(arb_note_status()),
            proptest::option::of(arb_yaml_safe_string()),
            proptest::collection::vec(arb_yaml_safe_string(), 0..4),
            proptest::collection::vec(arb_yaml_safe_string(), 0..3),
        )
            .prop_map(|(note_type, id, title, status, created, tags, aliases)| {
                Frontmatter {
                    id,
                    title,
                    note_type,
                    status,
                    created,
                    tags,
                    aliases,
                    source_url: None,
                    authors: vec![],
                    published: None,
                    challenges: None,
                    source_corpus: None,
                }
            })
    }

    proptest! {
        /// parse(serialize(fm)) == fm for any valid Frontmatter.
        #[test]
        fn round_trip_prop(fm in arb_frontmatter()) {
            let serialized = serialize_frontmatter(&fm);
            let parsed = parse_frontmatter(&serialized)
                .expect("serialize should always produce parseable output");
            prop_assert_eq!(fm, parsed);
        }

        /// The serialized output always starts and ends with the YAML delimiters.
        #[test]
        fn serialized_always_delimited(fm in arb_frontmatter()) {
            let out = serialize_frontmatter(&fm);
            prop_assert!(out.starts_with("---\n"));
            prop_assert!(out.ends_with("---\n"));
        }
    }
}
