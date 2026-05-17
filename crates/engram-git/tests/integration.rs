//! Integration tests for [`engram_git`] against a real, disposable git
//! repository created in a `tempfile::TempDir`.
//!
//! The harness uses the system `git` binary to populate the test repo —
//! exercising our gix-backed reads against ground truth produced by git
//! itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use engram_git::{Change, CommitOpts, ReadOnlyGit, WriteGit};
use tempfile::TempDir;

struct TestRepo {
    _dir: TempDir,
    path: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        run(&path, &["init", "-q", "-b", "main"]);
        run(&path, &["config", "user.name", "engram-test"]);
        run(&path, &["config", "user.email", "test@engram.local"]);
        run(&path, &["config", "commit.gpgsign", "false"]);
        Self { _dir: dir, path }
    }

    fn write(&self, rel: &str, body: &str) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(full, body).expect("write file");
    }

    fn commit_all(&self, msg: &str) {
        run(&self.path, &["add", "-A"]);
        run(&self.path, &["commit", "-q", "-m", msg]);
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn run(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .expect("invoke git");
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn open_returns_two_distinct_handle_types() {
    let repo = TestRepo::new();
    repo.write("seed.md", "seed\n");
    repo.commit_all("seed");

    let (write, read) = engram_git::open(repo.path()).expect("open");

    // Both implement ReadOnlyGit (compile-time check via trait-object cast).
    let _r: &dyn ReadOnlyGit = &read;
    let _w_as_r: &dyn ReadOnlyGit = &write;
    // Only WriteHandle implements WriteGit.
    let _w: &dyn WriteGit = &write;
}

#[test]
fn rev_parse_resolves_head() {
    let repo = TestRepo::new();
    repo.write("a.md", "x\n");
    repo.commit_all("first");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let sha = read.rev_parse("HEAD").expect("rev_parse HEAD");
    assert_eq!(sha.as_str().len(), 40, "full SHA-1 hex expected");
}

#[test]
fn rev_parse_unknown_ref_errors() {
    let repo = TestRepo::new();
    repo.write("a.md", "x\n");
    repo.commit_all("first");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let err = read.rev_parse("definitely-not-a-ref").unwrap_err();
    assert!(matches!(err, engram_git::Error::RevParse { .. }));
}

#[test]
fn log_returns_commits_in_reverse_chronological_order() {
    let repo = TestRepo::new();
    repo.write("a.md", "1\n");
    repo.commit_all("first");
    repo.write("b.md", "2\n");
    repo.commit_all("second");
    repo.write("c.md", "3\n");
    repo.commit_all("third");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let commits = read.log(None, 10).expect("log");
    assert_eq!(commits.len(), 3);

    // Walk yields newest first.
    let subjects: Vec<&str> = commits
        .iter()
        .map(|c| c.message.lines().next().unwrap_or(""))
        .collect();
    assert_eq!(subjects, vec!["third", "second", "first"]);
}

#[test]
fn log_respects_limit() {
    let repo = TestRepo::new();
    for i in 0..5 {
        repo.write("a.md", &format!("{i}\n"));
        repo.commit_all(&format!("c{i}"));
    }

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let commits = read.log(None, 2).expect("log");
    assert_eq!(commits.len(), 2);
}

#[test]
fn status_clean_after_commit() {
    let repo = TestRepo::new();
    repo.write("a.md", "x\n");
    repo.commit_all("seed");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let status = read.status().expect("status");
    assert!(status.clean(), "expected clean status, got {status:?}");
}

#[test]
fn status_surfaces_unstaged_and_untracked() {
    let repo = TestRepo::new();
    repo.write("tracked.md", "v1\n");
    repo.commit_all("seed");

    // Modify tracked + add untracked, do NOT commit.
    repo.write("tracked.md", "v2\n");
    repo.write("new.md", "fresh\n");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let status = read.status().expect("status");
    assert!(!status.clean());

    let has_modified = status
        .entries
        .iter()
        .any(|e| e.path == Path::new("tracked.md") && e.change == Change::Modified);
    let has_untracked = status
        .entries
        .iter()
        .any(|e| e.path == Path::new("new.md") && e.change == Change::Untracked);
    assert!(has_modified, "expected modified entry for tracked.md");
    assert!(has_untracked, "expected untracked entry for new.md");
}

#[test]
fn ls_tree_lists_root_entries() {
    let repo = TestRepo::new();
    repo.write("a.md", "1\n");
    repo.write("nested/b.md", "2\n");
    repo.commit_all("seed");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let entries = read.ls_tree("HEAD").expect("ls_tree HEAD");

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"a.md"));
    assert!(names.contains(&"nested"));
}

#[test]
fn show_returns_blob_bytes_at_commit() {
    let repo = TestRepo::new();
    repo.write("note.md", "hello world\n");
    repo.commit_all("seed");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let bytes = read
        .show("HEAD", &PathBuf::from("note.md"))
        .expect("show note.md @ HEAD");
    assert_eq!(bytes, b"hello world\n");
}

// ─── diff tests ───────────────────────────────────────────────────────────

#[test]
fn diff_path_returns_empty_patch_when_clean() {
    let repo = TestRepo::new();
    repo.write("note.md", "v1\n");
    repo.commit_all("seed");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let diff = read.diff(Path::new("note.md")).expect("diff");
    assert!(diff.patch.is_empty(), "no changes — patch should be empty");
}

#[test]
fn diff_path_contains_hunk_for_modified_file() {
    let repo = TestRepo::new();
    repo.write("note.md", "line one\n");
    repo.commit_all("seed");
    repo.write("note.md", "line one\nline two\n");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let diff = read.diff(Path::new("note.md")).expect("diff");
    assert!(
        diff.patch.contains("+line two"),
        "expected added line in patch: {diff:?}"
    );
}

#[test]
fn diff_index_empty_when_nothing_staged() {
    let repo = TestRepo::new();
    repo.write("a.md", "x\n");
    repo.commit_all("seed");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let diffs = read.diff_index().expect("diff_index");
    assert!(diffs.is_empty(), "nothing staged");
}

#[test]
fn diff_index_shows_staged_change() {
    let repo = TestRepo::new();
    repo.write("a.md", "v1\n");
    repo.commit_all("seed");
    repo.write("a.md", "v2\n");
    run(repo.path(), &["add", "a.md"]);

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let diffs = read.diff_index().expect("diff_index");
    assert_eq!(diffs.len(), 1);
    assert!(diffs[0].patch.contains("+v2"), "staged hunk expected");
}

#[test]
fn diff_worktree_empty_when_clean() {
    let repo = TestRepo::new();
    repo.write("a.md", "x\n");
    repo.commit_all("seed");

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let diffs = read.diff_worktree().expect("diff_worktree");
    assert!(diffs.is_empty(), "clean worktree");
}

#[test]
fn diff_worktree_shows_unstaged_change() {
    let repo = TestRepo::new();
    repo.write("a.md", "v1\n");
    repo.commit_all("seed");
    repo.write("a.md", "v2\n"); // not staged

    let (_, read) = engram_git::open(repo.path()).expect("open");
    let diffs = read.diff_worktree().expect("diff_worktree");
    assert_eq!(diffs.len(), 1);
    assert!(diffs[0].patch.contains("+v2"), "unstaged hunk expected");
}

// ─── write tests ──────────────────────────────────────────────────────────

#[test]
fn add_stages_file() {
    let repo = TestRepo::new();
    repo.write("a.md", "v1\n");
    repo.commit_all("seed");
    repo.write("a.md", "v2\n");

    let (write, _) = engram_git::open(repo.path()).expect("open");
    write.add(&[Path::new("a.md")]).expect("add");

    let staged = write.diff_index().expect("diff_index after add");
    assert_eq!(staged.len(), 1, "file should be staged");
}

#[test]
fn restore_discards_unstaged_change() {
    let repo = TestRepo::new();
    repo.write("a.md", "v1\n");
    repo.commit_all("seed");
    repo.write("a.md", "v2\n");

    let (write, _) = engram_git::open(repo.path()).expect("open");
    write.restore(&[Path::new("a.md")]).expect("restore");

    let unstaged = write.diff_worktree().expect("diff_worktree after restore");
    assert!(
        unstaged.is_empty(),
        "restore should discard worktree change"
    );
}

#[test]
fn commit_creates_new_head() {
    let repo = TestRepo::new();
    repo.write("a.md", "v1\n");
    repo.commit_all("seed");
    let (write, read) = engram_git::open(repo.path()).expect("open");
    let before = read.rev_parse("HEAD").expect("HEAD before");

    repo.write("a.md", "v2\n");
    write.add(&[Path::new("a.md")]).expect("add");
    let new_sha = write
        .commit("update a", CommitOpts::default())
        .expect("commit");

    assert_ne!(new_sha.as_str(), before.as_str(), "HEAD must advance");
    assert_eq!(new_sha.as_str().len(), 40);
}

#[test]
fn commit_appends_co_author_trailers() {
    let repo = TestRepo::new();
    repo.write("a.md", "v1\n");
    repo.commit_all("seed");
    let (write, read) = engram_git::open(repo.path()).expect("open");

    repo.write("a.md", "v2\n");
    write.add(&[Path::new("a.md")]).expect("add");
    write
        .commit(
            "update with trailer",
            CommitOpts {
                co_authors: vec!["Alice <alice@example.com>".to_string()],
                footer_lines: vec!["engram-actions: summarize".to_string()],
            },
        )
        .expect("commit");

    let commits = read.log(None, 1).expect("log");
    let msg = &commits[0].message;
    assert!(
        msg.contains("Co-authored-by: Alice <alice@example.com>"),
        "co-author trailer missing from: {msg}"
    );
    assert!(
        msg.contains("engram-actions: summarize"),
        "footer line missing from: {msg}"
    );
}

#[test]
fn build_commit_message_no_trailers() {
    // Verify the pure-function path — no subprocess needed.
    let msg = "simple message";
    let out = engram_git::testing::build_commit_message(msg, &CommitOpts::default());
    assert_eq!(out, msg);
}

#[test]
fn build_commit_message_with_trailers() {
    let opts = CommitOpts {
        co_authors: vec!["Bob <bob@example.com>".to_string()],
        footer_lines: vec!["x-custom: value".to_string()],
    };
    let out = engram_git::testing::build_commit_message("subject", &opts);
    assert!(out.contains("Co-authored-by: Bob <bob@example.com>"));
    assert!(out.contains("x-custom: value"));
    // Blank line separates body from trailer block.
    assert!(out.contains("\n\n"));
}
