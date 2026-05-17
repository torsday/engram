//! Filename collision detection at note-write time.
//!
//! When a writer (agent, Ingestor, Swift app, or Obsidian user) attempts to create
//! a note whose slug already exists in the target folder, this module appends the
//! smallest disambiguator (`-2`, `-3`, …) that resolves the conflict.
//!
//! ## Retry contract
//!
//! `resolve_collision` performs a read-only filesystem scan — it does **not** create
//! the file. There is an inherent TOCTOU window between this call and the actual
//! `fs::write`. Callers must handle [`std::io::ErrorKind::AlreadyExists`] by
//! re-calling `resolve_collision` with an incremented `hint` (or by calling
//! `resolve_collision_from` with the suffix that failed). In practice collisions are
//! extremely rare; a retry loop of 3–5 attempts is sufficient.
//!
//! Use `File::create_new` (O_CREAT | O_EXCL) so the OS returns `AlreadyExists`
//! atomically when a concurrent writer wins the race:
//!
//! ```text
//! loop {
//!     let slug = resolve_collision(&dir, &base_slug);
//!     let path = dir.join(format!("{slug}.md"));
//!     match std::fs::File::create_new(&path) {
//!         Ok(mut f) => { f.write_all(content)?; break; }
//!         Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
//!         Err(e) => return Err(e.into()),
//!     }
//! }
//! ```

use std::borrow::Cow;
use std::path::Path;

/// Return the slug to use when writing a new note into `target_dir`.
///
/// If `<target_dir>/<base_slug>.md` does not exist, returns `base_slug` unchanged.
/// Otherwise iterates suffixes starting at `-2` and returns the first free one.
///
/// The scan fills gaps: if `foo.md` and `foo-5.md` both exist but `foo-2.md` does
/// not, this returns `"foo-2"` rather than `"foo-6"`.
///
/// Does **not** create any files. See module-level docs for the retry contract.
pub fn resolve_collision<'a>(target_dir: &Path, base_slug: &'a str) -> Cow<'a, str> {
    if !target_dir.join(format!("{base_slug}.md")).exists() {
        return Cow::Borrowed(base_slug);
    }
    // Start at 2 (there is no "-1" suffix — the original is unsuffixed).
    let mut n = 2u32;
    loop {
        let candidate = format!("{base_slug}-{n}");
        if !target_dir.join(format!("{candidate}.md")).exists() {
            return Cow::Owned(candidate);
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), "").expect("touch");
    }

    // ── unit tests ───────────────────────────────────────────────────────────

    #[test]
    fn no_collision_returns_original() {
        let dir = setup();
        let result = resolve_collision(dir.path(), "foo");
        assert_eq!(result, "foo");
        // Must be borrowed (no allocation).
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn single_collision_returns_two() {
        let dir = setup();
        touch(dir.path(), "foo.md");
        assert_eq!(resolve_collision(dir.path(), "foo"), "foo-2");
    }

    #[test]
    fn multi_collision_sequential() {
        let dir = setup();
        touch(dir.path(), "foo.md");
        touch(dir.path(), "foo-2.md");
        touch(dir.path(), "foo-3.md");
        assert_eq!(resolve_collision(dir.path(), "foo"), "foo-4");
    }

    #[test]
    fn gap_fills_from_two() {
        // foo.md and foo-5.md exist; foo-2/3/4 do not → should return foo-2.
        let dir = setup();
        touch(dir.path(), "foo.md");
        touch(dir.path(), "foo-5.md");
        assert_eq!(resolve_collision(dir.path(), "foo"), "foo-2");
    }

    #[test]
    fn gap_fills_from_three() {
        let dir = setup();
        touch(dir.path(), "foo.md");
        touch(dir.path(), "foo-2.md");
        touch(dir.path(), "foo-5.md");
        // foo-3 is the first gap.
        assert_eq!(resolve_collision(dir.path(), "foo"), "foo-3");
    }

    #[test]
    fn different_slugs_do_not_interfere() {
        let dir = setup();
        touch(dir.path(), "bar.md");
        // "foo" has no collision; "bar" does.
        assert_eq!(resolve_collision(dir.path(), "foo"), "foo");
        assert_eq!(resolve_collision(dir.path(), "bar"), "bar-2");
    }

    #[test]
    fn slug_with_existing_numeric_suffix_is_treated_independently() {
        // "foo-2.md" exists but "foo.md" does not → no collision for base "foo".
        let dir = setup();
        touch(dir.path(), "foo-2.md");
        let result = resolve_collision(dir.path(), "foo");
        assert_eq!(result, "foo");
    }

    #[test]
    fn empty_dir_always_returns_original() {
        let dir = setup();
        assert_eq!(resolve_collision(dir.path(), "anything"), "anything");
    }

    #[test]
    fn original_slug_never_gets_one_suffix() {
        // Verify the design doc invariant: the first free name after base is "-2",
        // never "-1".
        let dir = setup();
        touch(dir.path(), "foo.md");
        let result = resolve_collision(dir.path(), "foo");
        assert_ne!(result, "foo-1", "design doc says no -1 suffix");
        assert_eq!(result, "foo-2");
    }

    // ── integration test: parallel writers ──────────────────────────────────

    #[test]
    fn parallel_writers_both_succeed_with_distinct_suffixes() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let dir = Arc::new(setup());
        let written: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Spawn N threads, each resolving and writing the same base slug.
        const WRITERS: usize = 8;
        let handles: Vec<_> = (0..WRITERS)
            .map(|_| {
                let dir = Arc::clone(&dir);
                let written = Arc::clone(&written);
                thread::spawn(move || {
                    // Retry loop matching the caller-side contract.
                    let mut attempts = 0;
                    loop {
                        attempts += 1;
                        assert!(attempts <= 50, "too many retries — logic error");
                        let slug = resolve_collision(dir.path(), "note");
                        let path = dir.path().join(format!("{slug}.md"));
                        // Use O_CREAT|O_EXCL so the OS returns AlreadyExists
                        // atomically when a concurrent writer wins the race.
                        match fs::File::create_new(&path) {
                            Ok(_) => {
                                written.lock().unwrap().push(slug.into_owned());
                                break;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                // Race: retry.
                                continue;
                            }
                            Err(e) => panic!("unexpected write error: {e}"),
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        let mut slugs = written.lock().unwrap().clone();
        slugs.sort();

        // All WRITERS succeeded with distinct slugs.
        assert_eq!(slugs.len(), WRITERS, "not all writers succeeded");
        let unique: std::collections::HashSet<_> = slugs.iter().collect();
        assert_eq!(unique.len(), WRITERS, "duplicate slugs produced");

        // Every slug is safe (no consecutive hyphens, no leading/trailing hyphen).
        for s in &slugs {
            assert!(!s.contains("--"));
            assert!(!s.starts_with('-'));
            assert!(!s.ends_with('-'));
        }
    }
}
