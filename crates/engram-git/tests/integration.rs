//! Integration tests for the read-side of [`engram_git`] against a real,
//! disposable git repository created in a `tempfile::TempDir`.
//!
//! The harness uses the system `git` binary to populate the test repo —
//! exercising our gix-backed reads against ground truth produced by git
//! itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use engram_git::{Change, ReadOnlyGit, WriteGit};
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

#[test]
fn write_methods_return_not_yet_implemented() {
    // The trait surface is locked even before each method is wired to gix.
    // Once a method is implemented this assertion will be flipped to verify
    // the success path; failing here is a healthy signal that the typed
    // placeholder is reachable from the WriteGit handle.
    let repo = TestRepo::new();
    repo.write("a.md", "x\n");
    repo.commit_all("seed");

    let (write, _) = engram_git::open(repo.path()).expect("open");
    let err = write
        .commit("msg", engram_git::CommitOpts::default())
        .unwrap_err();
    assert!(matches!(
        err,
        engram_git::Error::NotYetImplemented {
            method: "WriteGit::commit"
        }
    ));
}
