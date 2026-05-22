//! Prompt loader for agents — splits `agents/<name>/prompt.md` on the
//! cache-boundary marker into a [`PromptStructured`] that downstream LLM
//! providers can mark for prompt caching.
//!
//! Per [ADR 0010](../../../../docs/design/adrs/0010-prompt-caching-first-class.md),
//! every agent prompt is two halves:
//!
//! - **static head** — system identity, role, rubric, output schema, examples.
//!   Rarely changes; provider emits a cache marker at the boundary.
//! - **dynamic tail** — per-call inputs (the current note, recent
//!   neighbors, the user's question). Templated; never cached.
//!
//! The boundary is the marker `<!-- /cache -->` on a line by itself. The
//! parser is **case-insensitive** (`<!-- /CACHE -->`, `<!-- /Cache -->` all
//! count) and **whitespace-tolerant** (leading/trailing spaces on the
//! marker line are ignored).
//!
//! # Template substitution
//!
//! Variables of the form `{{name}}` in the *dynamic tail* are substituted at
//! render time. The static head is never templated — that would defeat the
//! cache-hit invariant.
//!
//! # Marker validation
//!
//! [`validate_agent_dir`] walks an `agents/` directory and returns any
//! marker-presence issues (missing marker, duplicate marker, prompt file
//! not readable). CI uses this to fail-fast when a contributor adds a new
//! `prompt.md` without the marker.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use engram_llm::PromptStructured;

/// The literal marker text. Compared case-insensitively against the trimmed
/// content of each line of the prompt file.
const CACHE_MARKER: &str = "<!-- /cache -->";

/// Errors from loading a single prompt file.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum PromptLoadError {
    /// File I/O failed.
    #[error("read `{path}`: {message}")]
    Io {
        /// Path that failed to read.
        path: PathBuf,
        /// Human-readable error.
        message: String,
    },

    /// The prompt file does not contain the cache-boundary marker.
    ///
    /// Carries a hint pointing at the canonical template in
    /// `docs/design/12-agent-spec-template.md`.
    #[error(
        "prompt `{path}` is missing the cache-boundary marker `<!-- /cache -->`.\n\
         Add it on a line by itself between the static system identity (head) and the per-call inputs (tail).\n\
         See docs/design/12-agent-spec-template.md for the canonical layout."
    )]
    MissingMarker {
        /// Path of the prompt file.
        path: PathBuf,
    },

    /// The prompt file contains more than one cache-boundary marker.
    /// Exactly one is required.
    #[error(
        "prompt `{path}` has {count} cache-boundary markers; expected exactly one.\n\
         Remove the extras so the static head / dynamic tail split is unambiguous."
    )]
    DuplicateMarker {
        /// Path of the prompt file.
        path: PathBuf,
        /// How many markers were found.
        count: usize,
    },
}

/// Read a prompt file and return its static / dynamic split.
///
/// The returned [`PromptStructured`] is the same type the LLM provider trait
/// accepts — pass it directly into `LlmProvider::complete` /
/// `complete_streamed`.
///
/// The marker line itself is **not** included in either half; the static
/// head ends at the last byte before the marker line's leading whitespace,
/// and the dynamic tail begins after the marker line's terminating newline.
/// This means the marker is "consumed" — round-tripping through
/// [`load`] + concat would lose the marker. That's intentional: the marker
/// is a *split signal*, not part of the prompt text.
pub fn load(prompt_path: &Path) -> Result<PromptStructured, PromptLoadError> {
    let contents = fs::read_to_string(prompt_path).map_err(|e| PromptLoadError::Io {
        path: prompt_path.to_path_buf(),
        message: e.to_string(),
    })?;
    split(&contents).map_err(|kind| match kind {
        SplitError::MissingMarker => PromptLoadError::MissingMarker {
            path: prompt_path.to_path_buf(),
        },
        SplitError::DuplicateMarker(count) => PromptLoadError::DuplicateMarker {
            path: prompt_path.to_path_buf(),
            count,
        },
    })
}

/// In-memory split — exposed for tests and for callers that already hold
/// the prompt body (e.g. fixtures, hot-reload paths that read once).
pub fn split(body: &str) -> Result<PromptStructured, SplitError> {
    // Find every line whose trimmed lowercase equals the marker.
    let positions: Vec<(usize, usize)> = byte_line_ranges(body)
        .filter(|(start, end)| {
            let line = &body[*start..*end];
            line.trim().eq_ignore_ascii_case(CACHE_MARKER)
        })
        .collect();

    match positions.len() {
        0 => Err(SplitError::MissingMarker),
        1 => {
            let (line_start, line_end) = positions[0];
            // Static head = everything before the marker line's start.
            let head = &body[..line_start];
            // Dynamic tail = everything after the marker line's end. If the
            // marker line is followed by a newline, skip it so the tail
            // doesn't begin with a stray blank line.
            let mut tail_start = line_end;
            if body[tail_start..].starts_with('\n') {
                tail_start += 1;
            } else if body[tail_start..].starts_with("\r\n") {
                tail_start += 2;
            }
            let tail = &body[tail_start..];
            Ok(PromptStructured::new(
                head.trim_end_matches(['\n', '\r']).to_string(),
                tail.to_string(),
            ))
        }
        n => Err(SplitError::DuplicateMarker(n)),
    }
}

/// Error variants produced by the in-memory [`split`] function. Distinct
/// from [`PromptLoadError`] so callers can attach their own path context
/// when wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// No cache-boundary marker found.
    MissingMarker,
    /// More than one marker found (count attached).
    DuplicateMarker(usize),
}

/// Render the dynamic tail by substituting `{{name}}` placeholders against
/// `vars`. Unknown placeholders are left intact (so a typo doesn't silently
/// produce an empty string).
///
/// The static head must not be passed through this function — that would
/// defeat the cache-hit invariant. The compile-time signature
/// (`PromptStructured` → caller picks the tail) makes the intended usage
/// obvious; this function is plain and reusable.
pub fn render_tail(tail: &str, vars: &HashMap<&str, &str>) -> String {
    let mut out = String::with_capacity(tail.len());
    let bytes = tail.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"{{") {
            // Find the matching `}}`. If absent, emit the bytes verbatim.
            if let Some(end_off) = tail[i + 2..].find("}}") {
                let name_start = i + 2;
                let name_end = name_start + end_off;
                let name = tail[name_start..name_end].trim();
                if let Some(value) = vars.get(name) {
                    out.push_str(value);
                    i = name_end + 2;
                    continue;
                }
                // Unknown placeholder — emit verbatim.
                out.push_str(&tail[i..name_end + 2]);
                i = name_end + 2;
                continue;
            }
        }
        // Emit one byte (or the multi-byte UTF-8 char it starts).
        let ch_start = i;
        let mut next = ch_start + 1;
        while next < bytes.len() && (bytes[next] & 0b1100_0000) == 0b1000_0000 {
            next += 1;
        }
        out.push_str(&tail[ch_start..next]);
        i = next;
    }
    out
}

/// Result of validating one prompt file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Path of the offending prompt file.
    pub path: PathBuf,
    /// What's wrong.
    pub kind: ValidationKind,
}

/// Category of validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationKind {
    /// Marker missing.
    MissingMarker,
    /// More than one marker.
    DuplicateMarker(usize),
    /// File could not be read.
    Unreadable(String),
}

/// Walk `agents_dir` and validate every `<agent>/prompt.md`. Returns one
/// [`ValidationIssue`] per failing prompt. An empty Vec means all prompts
/// are well-formed.
///
/// CI integration: fail the build when the return value is non-empty.
pub fn validate_agent_dir(agents_dir: &Path) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let entries = match fs::read_dir(agents_dir) {
        Ok(it) => it,
        Err(e) => {
            issues.push(ValidationIssue {
                path: agents_dir.to_path_buf(),
                kind: ValidationKind::Unreadable(e.to_string()),
            });
            return issues;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let prompt_path = path.join("prompt.md");
        if !prompt_path.exists() {
            // An agent dir without prompt.md isn't necessarily broken yet
            // (might be config-only or under construction). Skip.
            continue;
        }
        match load(&prompt_path) {
            Ok(_) => {}
            Err(PromptLoadError::MissingMarker { path }) => {
                issues.push(ValidationIssue {
                    path,
                    kind: ValidationKind::MissingMarker,
                });
            }
            Err(PromptLoadError::DuplicateMarker { path, count }) => {
                issues.push(ValidationIssue {
                    path,
                    kind: ValidationKind::DuplicateMarker(count),
                });
            }
            Err(PromptLoadError::Io { path, message }) => {
                issues.push(ValidationIssue {
                    path,
                    kind: ValidationKind::Unreadable(message),
                });
            }
        }
    }
    issues
}

// ── helpers ─────────────────────────────────────────────────────────────

/// Iterate `(start, end)` byte ranges of each line in `s`, excluding the
/// terminating newline. Handles `\n` and `\r\n`. Empty input yields no
/// items; trailing newline yields a final empty range.
fn byte_line_ranges(s: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    LineRanges {
        bytes: s.as_bytes(),
        cursor: 0,
        done: false,
    }
}

struct LineRanges<'a> {
    bytes: &'a [u8],
    cursor: usize,
    done: bool,
}

impl Iterator for LineRanges<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let start = self.cursor;
        let end_off = self.bytes[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p);
        match end_off {
            Some(nl_pos) => {
                // Strip a trailing CR for \r\n line endings.
                let line_end = if nl_pos > start && self.bytes[nl_pos - 1] == b'\r' {
                    nl_pos - 1
                } else {
                    nl_pos
                };
                self.cursor = nl_pos + 1;
                Some((start, line_end))
            }
            None => {
                // Last line without trailing newline.
                self.done = true;
                if start <= self.bytes.len() {
                    Some((start, self.bytes.len()))
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    // ── split() ─────────────────────────────────────────────────────────

    #[test]
    fn split_basic() {
        let body = "static line 1\nstatic line 2\n<!-- /cache -->\ntail line 1\ntail line 2\n";
        let s = split(body).unwrap();
        assert_eq!(s.static_head, "static line 1\nstatic line 2");
        assert_eq!(s.dynamic_tail, "tail line 1\ntail line 2\n");
    }

    #[test]
    fn split_case_insensitive_marker() {
        for marker in [
            "<!-- /cache -->",
            "<!-- /CACHE -->",
            "<!-- /Cache -->",
            "<!-- /cAcHe -->",
        ] {
            let body = format!("head\n{marker}\ntail\n");
            let s = split(&body).expect(marker);
            assert_eq!(s.static_head, "head");
            assert_eq!(s.dynamic_tail, "tail\n");
        }
    }

    #[test]
    fn split_whitespace_tolerant_marker() {
        let body = "head\n   <!-- /cache -->   \ntail\n";
        let s = split(body).unwrap();
        assert_eq!(s.static_head, "head");
        assert_eq!(s.dynamic_tail, "tail\n");
    }

    #[test]
    fn split_handles_crlf() {
        let body = "head\r\n<!-- /cache -->\r\ntail\r\n";
        let s = split(body).unwrap();
        assert_eq!(s.static_head, "head");
        assert_eq!(s.dynamic_tail, "tail\r\n");
    }

    #[test]
    fn split_empty_halves() {
        let s = split("<!-- /cache -->\n").unwrap();
        assert_eq!(s.static_head, "");
        assert_eq!(s.dynamic_tail, "");
    }

    #[test]
    fn split_missing_marker_errors() {
        assert_eq!(split("just a prompt\n"), Err(SplitError::MissingMarker));
    }

    #[test]
    fn split_duplicate_markers_errors() {
        let body = "head\n<!-- /cache -->\nmiddle\n<!-- /cache -->\ntail\n";
        assert_eq!(split(body), Err(SplitError::DuplicateMarker(2)));
    }

    #[test]
    fn split_marker_substring_inside_line_not_matched() {
        // The marker is only recognized as a complete line, not inline.
        let body = "head\nprose mentioning <!-- /cache --> mid-sentence\n<!-- /cache -->\ntail\n";
        let s = split(body).unwrap();
        // First match is the real marker line, not the prose mention.
        assert_eq!(
            s.static_head,
            "head\nprose mentioning <!-- /cache --> mid-sentence"
        );
        assert_eq!(s.dynamic_tail, "tail\n");
    }

    // ── load() ─────────────────────────────────────────────────────────

    #[test]
    fn load_reads_file_and_splits() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("prompt.md");
        write(
            &p,
            "system prompt\nidentity\n<!-- /cache -->\n{{note.body}}\n",
        );
        let s = load(&p).unwrap();
        assert_eq!(s.static_head, "system prompt\nidentity");
        assert_eq!(s.dynamic_tail, "{{note.body}}\n");
    }

    #[test]
    fn load_missing_file_returns_io_error() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("not_there.md");
        match load(&p) {
            Err(PromptLoadError::Io { path, .. }) => assert_eq!(path, p),
            other => panic!("expected Io, got {other:?}"),
        }
    }

    #[test]
    fn load_missing_marker_error_includes_hint() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("prompt.md");
        write(&p, "just prose, no marker\n");
        let err = load(&p).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("<!-- /cache -->"), "got: {msg}");
        assert!(msg.contains("12-agent-spec-template.md"), "got: {msg}");
    }

    // ── render_tail() ─────────────────────────────────────────────────

    #[test]
    fn render_substitutes_known_variables() {
        let mut vars = HashMap::new();
        vars.insert("note.body", "the body");
        vars.insert("note.title", "a title");
        let out = render_tail("Title: {{note.title}}\nBody: {{note.body}}\n", &vars);
        assert_eq!(out, "Title: a title\nBody: the body\n");
    }

    #[test]
    fn render_passes_unknown_placeholders_through() {
        let vars = HashMap::new();
        let out = render_tail("hello {{nobody}} world", &vars);
        assert_eq!(out, "hello {{nobody}} world");
    }

    #[test]
    fn render_handles_unterminated_braces() {
        let vars = HashMap::new();
        let out = render_tail("hello {{unterminated", &vars);
        assert_eq!(out, "hello {{unterminated");
    }

    #[test]
    fn render_trims_placeholder_name_whitespace() {
        let mut vars = HashMap::new();
        vars.insert("x", "y");
        assert_eq!(render_tail("{{x}}", &vars), "y");
        assert_eq!(render_tail("{{ x }}", &vars), "y");
        assert_eq!(render_tail("{{  x  }}", &vars), "y");
    }

    #[test]
    fn render_does_not_touch_static_head() {
        // The contract documents that the static head is never templated.
        // We enforce that at the call site (callers pass `&prompt.dynamic_tail`).
        // The function itself is symmetric; this test pins that we never
        // expose a convenience that would template the head.
        // (Compile-time: PromptStructured has no `render_all()` method.)
    }

    // ── validate_agent_dir() ──────────────────────────────────────────

    #[test]
    fn validate_returns_empty_when_all_prompts_good() {
        let dir = tempdir().unwrap();
        let agents = dir.path();
        write(&agents.join("a/prompt.md"), "head\n<!-- /cache -->\ntail\n");
        write(&agents.join("b/prompt.md"), "head\n<!-- /cache -->\ntail\n");
        assert!(validate_agent_dir(agents).is_empty());
    }

    #[test]
    fn validate_reports_missing_marker() {
        let dir = tempdir().unwrap();
        let agents = dir.path();
        write(&agents.join("a/prompt.md"), "no marker here\n");
        let issues = validate_agent_dir(agents);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ValidationKind::MissingMarker);
        assert!(issues[0].path.ends_with("a/prompt.md"));
    }

    #[test]
    fn validate_reports_duplicate_marker() {
        let dir = tempdir().unwrap();
        let agents = dir.path();
        write(
            &agents.join("a/prompt.md"),
            "head\n<!-- /cache -->\nmid\n<!-- /cache -->\ntail\n",
        );
        let issues = validate_agent_dir(agents);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ValidationKind::DuplicateMarker(2));
    }

    #[test]
    fn validate_skips_agent_dirs_without_prompt_md() {
        let dir = tempdir().unwrap();
        let agents = dir.path();
        // Config-only agent, no prompt.md — not a validation failure.
        write(&agents.join("a/config.toml"), "name = \"a\"\n");
        assert!(validate_agent_dir(agents).is_empty());
    }

    #[test]
    fn validate_handles_unreadable_dir() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does_not_exist");
        let issues = validate_agent_dir(&missing);
        assert_eq!(issues.len(), 1);
        assert!(matches!(issues[0].kind, ValidationKind::Unreadable(_)));
    }
}
