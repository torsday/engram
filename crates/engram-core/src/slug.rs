//! Title-to-slug normalization for engram note filenames.
//!
//! Rules (per `docs/design/06-note-conventions.md` §Filename rules):
//! - Lowercase
//! - Spaces → hyphens
//! - Strip punctuation except hyphens
//! - Collapse consecutive hyphens
//! - Truncate to 80 chars (on a char boundary)
//! - Strip leading/trailing hyphens
//! - Empty result → `"untitled"`

use deunicode::deunicode_char;

const MAX_SLUG_LEN: usize = 80;

/// Convert a note title to a filesystem-safe slug.
///
/// The transformation is deterministic and idempotent: `slugify(slugify(s)) == slugify(s)`.
///
/// An empty or whitespace-only title, or one whose characters all transliterate to
/// nothing useful, returns `"untitled"`.
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());

    for ch in title.chars() {
        if ch.is_ascii() {
            push_ascii(&mut slug, ch);
        } else {
            // Transliterate non-ASCII to ASCII equivalent(s), then process each char.
            if let Some(ascii) = deunicode_char(ch) {
                for a in ascii.chars() {
                    push_ascii(&mut slug, a);
                }
            }
            // Characters with no transliteration are silently dropped.
        }
    }

    // Collapse consecutive hyphens produced by stripping punctuation runs.
    let collapsed = collapse_hyphens(&slug);

    // Strip leading/trailing hyphens.
    let trimmed = collapsed.trim_matches('-');

    // Truncate to MAX_SLUG_LEN chars, then strip any trailing hyphen the cut may expose.
    let truncated = truncate_chars(trimmed, MAX_SLUG_LEN);
    let result = truncated.trim_end_matches('-');

    if result.is_empty() {
        "untitled".to_owned()
    } else {
        result.to_owned()
    }
}

/// Map a single ASCII character to its slug form, appending to `out`.
#[inline]
fn push_ascii(out: &mut String, ch: char) {
    if ch.is_ascii_alphanumeric() {
        out.push(ch.to_ascii_lowercase());
    } else if ch == ' ' || ch == '-' || ch == '_' {
        // Treat underscores and spaces the same as hyphens.
        out.push('-');
    }
    // Everything else (punctuation, control chars) is dropped.
}

/// Collapse runs of consecutive hyphens into a single hyphen.
fn collapse_hyphens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_hyphen = false;
    for ch in s.chars() {
        if ch == '-' {
            if !prev_hyphen {
                out.push('-');
            }
            prev_hyphen = true;
        } else {
            out.push(ch);
            prev_hyphen = false;
        }
    }
    out
}

/// Truncate `s` to at most `max_chars` characters (Unicode char count, not bytes).
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    // Find the byte offset of the max_chars-th char boundary.
    let byte_end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..byte_end]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── unit tests ───────────────────────────────────────────────────────────

    #[test]
    fn simple_title() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn punctuation_stripped() {
        assert_eq!(slugify("Foo, Bar! Baz?"), "foo-bar-baz");
    }

    #[test]
    fn accented_characters_transliterated() {
        // deunicode: ü→u, ß→ss
        assert_eq!(slugify("Über die Straße"), "uber-die-strasse");
    }

    #[test]
    fn unicode_cjk_drops_gracefully() {
        // CJK may transliterate to nothing — result should be untitled or whatever
        // romanisation deunicode gives; must not panic or produce invalid slugs.
        let s = slugify("日本語");
        assert!(!s.contains(' '));
        assert!(!s.starts_with('-'));
        assert!(!s.ends_with('-'));
        assert!(!s.contains("--"));
    }

    #[test]
    fn consecutive_hyphens_collapsed() {
        assert_eq!(slugify("foo---bar"), "foo-bar");
        assert_eq!(slugify("foo  bar"), "foo-bar");
        assert_eq!(slugify("foo!!! bar"), "foo-bar");
    }

    #[test]
    fn leading_trailing_hyphens_stripped() {
        assert_eq!(slugify("  hello  "), "hello");
        assert_eq!(slugify("!hello!"), "hello");
        assert_eq!(slugify("-hello-"), "hello");
    }

    #[test]
    fn very_long_title_truncated() {
        let long = "a".repeat(200);
        let result = slugify(&long);
        assert!(result.chars().count() <= 80);
    }

    #[test]
    fn truncation_does_not_leave_trailing_hyphen() {
        // Build a title that would produce a hyphen right at the boundary.
        let title = format!("{}! end", "a".repeat(79));
        let result = slugify(&title);
        assert!(result.chars().count() <= 80);
        assert!(!result.ends_with('-'));
    }

    #[test]
    fn empty_title_returns_untitled() {
        assert_eq!(slugify(""), "untitled");
    }

    #[test]
    fn whitespace_only_returns_untitled() {
        assert_eq!(slugify("   "), "untitled");
    }

    #[test]
    fn pure_punctuation_returns_untitled() {
        assert_eq!(slugify("!!! ???"), "untitled");
    }

    #[test]
    fn already_a_slug_unchanged() {
        assert_eq!(slugify("hello-world"), "hello-world");
        assert_eq!(slugify("foo-bar-baz"), "foo-bar-baz");
    }

    #[test]
    fn numbers_preserved() {
        assert_eq!(slugify("Chapter 42"), "chapter-42");
        assert_eq!(slugify("2026-04-17"), "2026-04-17");
    }

    #[test]
    fn underscores_treated_as_hyphens() {
        assert_eq!(slugify("foo_bar"), "foo-bar");
    }

    #[test]
    fn idempotent_simple() {
        let cases = ["Hello World", "foo-bar", "!test!", "Über die Straße", ""];
        for &c in &cases {
            let once = slugify(c);
            let twice = slugify(&once);
            assert_eq!(once, twice, "not idempotent for {:?}", c);
        }
    }

    // ── property tests ───────────────────────────────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// slugify is idempotent: applying it twice yields the same result.
        #[test]
        fn prop_idempotent(s in ".*") {
            let once = slugify(&s);
            let twice = slugify(&once);
            prop_assert_eq!(&once, &twice);
        }

        /// Output never contains uppercase letters.
        #[test]
        fn prop_no_uppercase(s in ".*") {
            let result = slugify(&s);
            prop_assert!(result.chars().all(|c| !c.is_uppercase()),
                "found uppercase in {:?}", result);
        }

        /// Output never starts or ends with a hyphen (unless it is "untitled").
        #[test]
        fn prop_no_leading_trailing_hyphen(s in ".*") {
            let result = slugify(&s);
            if result != "untitled" {
                prop_assert!(!result.starts_with('-'),
                    "starts with hyphen: {:?}", result);
                prop_assert!(!result.ends_with('-'),
                    "ends with hyphen: {:?}", result);
            }
        }

        /// Output never contains consecutive hyphens.
        #[test]
        fn prop_no_consecutive_hyphens(s in ".*") {
            let result = slugify(&s);
            prop_assert!(!result.contains("--"),
                "consecutive hyphens in {:?}", result);
        }

        /// Output never exceeds 80 characters.
        #[test]
        fn prop_max_length(s in ".*") {
            let result = slugify(&s);
            prop_assert!(result.chars().count() <= 80,
                "too long ({} chars): {:?}", result.chars().count(), result);
        }

        /// Output contains only ASCII alphanumeric characters and hyphens.
        #[test]
        fn prop_only_safe_chars(s in ".*") {
            let result = slugify(&s);
            prop_assert!(result.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "unsafe char in {:?}", result);
        }
    }
}
