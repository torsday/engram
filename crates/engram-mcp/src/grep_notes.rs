//! `grep_notes` MCP tool — exact-string or regex lookup across vault markdown.
//!
//! Distinct from `search_notes` — no embeddings, no ranking, no relevance
//! scoring. Deterministic substring or regex match. Useful when the user knows
//! the literal phrase, ID, or pattern they are looking for.
//!
//! ## Input schema
//!
//! ```json
//! {
//!   "pattern":        "<string>",      // required
//!   "regex":          false,           // optional, default false
//!   "case_sensitive": false,           // optional, default false
//!   "max_matches":    100              // optional, default 100
//! }
//! ```
//!
//! ## Output schema
//!
//! ```json
//! {
//!   "matches": [
//!     {
//!       "note_id":     "<ULID or empty string if no frontmatter>",
//!       "path":        "<absolute path to .md file>",
//!       "line_number": 1,
//!       "line_text":   "<the matching line>",
//!       "char_offset": 5
//!     }
//!   ]
//! }
//! ```
//!
//! ## Error codes
//!
//! | code                   | meaning                                    |
//! |------------------------|--------------------------------------------|
//! | `bad_input`            | Empty pattern or invalid regex             |
//! | `vault_not_configured` | Vault root is not a directory              |
//! | `io_error`             | I/O failure scanning the vault             |

use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input for the `grep_notes` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct GrepNotesInput {
    /// Pattern to search for.
    pub pattern: String,
    /// Treat `pattern` as a regular expression.
    #[serde(default)]
    pub regex: bool,
    /// Case-sensitive match (default: false).
    #[serde(default)]
    pub case_sensitive: bool,
    /// Maximum number of matches to return (default: 100).
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
}

fn default_max_matches() -> usize {
    100
}

/// One match record.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MatchRecord {
    /// ULID from frontmatter, or empty string if the file has no frontmatter.
    pub note_id: String,
    /// Absolute path to the `.md` file.
    pub path: String,
    /// 1-based line number.
    pub line_number: usize,
    /// The full text of the matching line.
    pub line_text: String,
    /// 0-based byte offset of the first match within `line_text`.
    pub char_offset: usize,
}

/// Successful output for the `grep_notes` tool.
#[derive(Debug, Clone, Serialize)]
pub struct GrepNotesOutput {
    /// All matches, capped at `max_matches`.
    pub matches: Vec<MatchRecord>,
}

/// MCP-shaped error response.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolError {
    /// Stable string error code (snake_case).
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Public handler
// ---------------------------------------------------------------------------

/// Execute the `grep_notes` tool.
pub fn handle(vault_root: &Path, input: GrepNotesInput) -> Result<GrepNotesOutput, ToolError> {
    if input.pattern.is_empty() {
        return Err(ToolError {
            code: "bad_input".into(),
            message: "pattern must not be empty".into(),
        });
    }

    if !vault_root.is_dir() {
        return Err(ToolError {
            code: "vault_not_configured".into(),
            message: format!("vault root is not a directory: {}", vault_root.display()),
        });
    }

    let matcher = build_matcher(&input)?;
    let mut matches = Vec::new();

    search_dir(vault_root, &matcher, &mut matches, input.max_matches).map_err(|e| ToolError {
        code: "io_error".into(),
        message: format!("vault scan error: {e}"),
    })?;

    Ok(GrepNotesOutput { matches })
}

// ---------------------------------------------------------------------------
// Matcher abstraction
// ---------------------------------------------------------------------------

enum Matcher {
    Literal {
        pattern: String,
        case_sensitive: bool,
    },
    Regex(regex::Regex),
}

impl Matcher {
    fn find(&self, haystack: &str) -> Option<usize> {
        match self {
            Matcher::Literal {
                pattern,
                case_sensitive,
            } => {
                if *case_sensitive {
                    haystack.find(pattern.as_str())
                } else {
                    haystack
                        .to_lowercase()
                        .find(pattern.to_lowercase().as_str())
                }
            }
            Matcher::Regex(re) => re.find(haystack).map(|m| m.start()),
        }
    }
}

fn build_matcher(input: &GrepNotesInput) -> Result<Matcher, ToolError> {
    if input.regex {
        let flags = if input.case_sensitive { "" } else { "(?i)" };
        let full = format!("{}{}", flags, input.pattern);
        let re = regex::Regex::new(&full).map_err(|e| ToolError {
            code: "bad_input".into(),
            message: format!("invalid regex: {e}"),
        })?;
        Ok(Matcher::Regex(re))
    } else {
        Ok(Matcher::Literal {
            pattern: input.pattern.clone(),
            case_sensitive: input.case_sensitive,
        })
    }
}

// ---------------------------------------------------------------------------
// Vault walker
// ---------------------------------------------------------------------------

fn search_dir(
    dir: &Path,
    matcher: &Matcher,
    matches: &mut Vec<MatchRecord>,
    max: usize,
) -> std::io::Result<()> {
    if matches.len() >= max {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        if matches.len() >= max {
            break;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            search_dir(&path, matcher, matches, max)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            search_file(&path, matcher, matches, max)?;
        }
    }
    Ok(())
}

fn search_file(
    path: &Path,
    matcher: &Matcher,
    matches: &mut Vec<MatchRecord>,
    max: usize,
) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;

    // Extract note_id from frontmatter (best-effort; empty string on failure).
    let note_id = extract_id(&content);

    for (idx, line) in content.lines().enumerate() {
        if matches.len() >= max {
            break;
        }
        if let Some(offset) = matcher.find(line) {
            matches.push(MatchRecord {
                note_id: note_id.clone(),
                path: path.to_string_lossy().into_owned(),
                line_number: idx + 1,
                line_text: line.to_owned(),
                char_offset: offset,
            });
        }
    }
    Ok(())
}

/// Best-effort extraction of `id:` from YAML frontmatter.
fn extract_id(content: &str) -> String {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("id:") {
            return rest.trim().to_owned();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_vault() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("note-a.md"),
            "---\nid: AAAA\ntitle: A\ntype: fleeting\n---\n\nHello World\nfoo bar\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("note-b.md"),
            "---\nid: BBBB\ntitle: B\ntype: fleeting\n---\n\nhello lowercase\nno match here\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn literal_case_insensitive_finds_both() {
        let vault = make_vault();
        let out = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "hello".into(),
                regex: false,
                case_sensitive: false,
                max_matches: 100,
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 2);
    }

    #[test]
    fn literal_case_sensitive_finds_one() {
        let vault = make_vault();
        let out = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "Hello".into(),
                regex: false,
                case_sensitive: true,
                max_matches: 100,
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].line_text, "Hello World");
        assert_eq!(out.matches[0].char_offset, 0);
    }

    #[test]
    fn regex_match_works() {
        let vault = make_vault();
        let out = handle(
            vault.path(),
            GrepNotesInput {
                pattern: r"foo\s+bar".into(),
                regex: true,
                case_sensitive: false,
                max_matches: 100,
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].line_text, "foo bar");
    }

    #[test]
    fn max_matches_respected() {
        let vault = make_vault();
        let out = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "hello".into(),
                regex: false,
                case_sensitive: false,
                max_matches: 1,
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 1);
    }

    #[test]
    fn empty_pattern_returns_bad_input() {
        let vault = make_vault();
        let err = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "".into(),
                regex: false,
                case_sensitive: false,
                max_matches: 100,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn invalid_regex_returns_bad_input() {
        let vault = make_vault();
        let err = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "[invalid".into(),
                regex: true,
                case_sensitive: false,
                max_matches: 100,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_input");
    }

    #[test]
    fn note_id_extracted_from_frontmatter() {
        let vault = make_vault();
        let out = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "Hello World".into(),
                regex: false,
                case_sensitive: true,
                max_matches: 100,
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].note_id, "AAAA");
    }

    #[test]
    fn output_schema_has_all_fields() {
        let vault = make_vault();
        let out = handle(
            vault.path(),
            GrepNotesInput {
                pattern: "hello".into(),
                regex: false,
                case_sensitive: false,
                max_matches: 1,
            },
        )
        .unwrap();
        let json = serde_json::to_value(&out).unwrap();
        assert!(json.get("matches").is_some());
        let m = &json["matches"][0];
        for field in ["note_id", "path", "line_number", "line_text", "char_offset"] {
            assert!(m.get(field).is_some(), "missing field: {field}");
        }
    }
}
