//! Concrete handle types and the [`open`] constructor.
//!
//! [`WriteHandle`] and [`ReadHandle`] are deliberately distinct nominal types
//! sharing the same backing `gix::ThreadSafeRepository`. This means:
//!
//! - `&WriteHandle` can be passed where `&dyn WriteGit` or `&dyn ReadOnlyGit`
//!   is expected (since `WriteGit: ReadOnlyGit`).
//! - `&ReadHandle` can only be passed where `&dyn ReadOnlyGit` is expected.
//! - There is no safe path from `ReadHandle` to `WriteHandle` — the two are
//!   not related by trait inheritance, only by sharing an internal repo.
//!
//! [`open`] is the only public way to obtain a [`WriteHandle`].

use std::path::{Path, PathBuf};

use gix::ThreadSafeRepository;

use crate::error::{Error, Result};
use crate::traits::{ReadOnlyGit, WriteGit};
use crate::types::{Commit, CommitOpts, Diff, FileDiff, Sha, Status, TreeEntry};

/// Open the repository at `path`, returning both a [`WriteHandle`] for the
/// HTTP/CLI layers and a [`ReadHandle`] for the agent runner.
///
/// The two handles share the same underlying `gix::ThreadSafeRepository` —
/// state observed through either is consistent.
pub fn open(path: &Path) -> Result<(WriteHandle, ReadHandle)> {
    let inner = ThreadSafeRepository::open(path).map_err(|e| Error::Open {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let inner_clone = inner.clone();
    Ok((WriteHandle { inner }, ReadHandle { inner: inner_clone }))
}

/// Mutating handle. Implements [`WriteGit`] (and therefore [`ReadOnlyGit`]).
///
/// Distinct from [`ReadHandle`]: no safe conversion exists.
#[derive(Clone)]
pub struct WriteHandle {
    inner: ThreadSafeRepository,
}

/// Read-only handle. Implements [`ReadOnlyGit`] only.
///
/// Distinct from [`WriteHandle`]: no safe conversion exists.
#[derive(Clone)]
pub struct ReadHandle {
    inner: ThreadSafeRepository,
}

// ─── Shared read implementation ────────────────────────────────────────────
//
// Both handles delegate read operations through this private impl so the two
// trait impls below stay one-liners.

fn workdir(repo: &gix::Repository) -> Result<PathBuf> {
    repo.workdir()
        .ok_or_else(|| Error::Status("repository has no working tree (bare?)".to_string()))
        .map(|p| p.to_path_buf())
}

fn git_cmd(cwd: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke git: {e}"))
}

fn git_cmd_success(cwd: &Path, args: &[&str], error_kind: impl Fn(String) -> Error) -> Result<()> {
    let out = git_cmd(cwd, args);
    if out.status.success() {
        Ok(())
    } else {
        Err(error_kind(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }
}

fn read_status(repo: &gix::Repository) -> Result<Status> {
    use crate::types::StatusEntry;

    // gix-status reached 0.30.0 in gix 0.83 but its high-level API still
    // requires wiring through gix::status::Platform which doesn't yet cover
    // the untracked-files case in the same call as staged diffs. Keeping the
    // subprocess path until the API stabilises. Track:
    // https://github.com/Byron/gitoxide/issues/XXX (gix-status untracked)
    let dir = workdir(repo)?;
    let output = git_cmd(&dir, &["status", "--porcelain=v1", "-z"]);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Status(format!(
            "git status failed ({}): {stderr}",
            output.status
        )));
    }

    let mut entries = Vec::new();
    // Porcelain v1 with -z: NUL-separated records. Each record is "XY path"
    // where XY are two status codes; renames are "R  old\0new" — the rename
    // case is uncommon enough that we record it as Renamed and surface the
    // new path only. Sufficient for the diff-review queue's needs.
    for record in output.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if record.len() < 4 {
            continue;
        }
        let staged_code = record[0];
        let worktree_code = record[1];
        // record[2] is a space separator
        let path_bytes = &record[3..];
        let path = PathBuf::from(std::ffi::OsStr::from_bytes_lossy(path_bytes).into_owned());

        if staged_code != b' ' && staged_code != b'?' {
            entries.push(StatusEntry {
                path: path.clone(),
                change: code_to_change(staged_code),
                staged: true,
            });
        }
        if worktree_code != b' ' {
            entries.push(StatusEntry {
                path,
                change: code_to_change(worktree_code),
                staged: false,
            });
        }
    }

    Ok(Status { entries })
}

fn code_to_change(code: u8) -> crate::types::Change {
    use crate::types::Change;
    match code {
        b'A' => Change::Added,
        b'M' => Change::Modified,
        b'D' => Change::Deleted,
        b'R' => Change::Renamed,
        b'?' => Change::Untracked,
        // Treat unmapped codes (C, U, T) as Modified — good enough for the
        // review queue, which only cares that something changed.
        _ => Change::Modified,
    }
}

trait OsStrExt {
    fn from_bytes_lossy(bytes: &[u8]) -> std::borrow::Cow<'_, std::ffi::OsStr>;
}

impl OsStrExt for std::ffi::OsStr {
    fn from_bytes_lossy(bytes: &[u8]) -> std::borrow::Cow<'_, std::ffi::OsStr> {
        // On Unix, OsStr is bytes; on Windows, fall back through UTF-8.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            std::borrow::Cow::Borrowed(std::ffi::OsStr::from_bytes(bytes))
        }
        #[cfg(not(unix))]
        {
            std::borrow::Cow::Owned(std::ffi::OsString::from(
                String::from_utf8_lossy(bytes).into_owned(),
            ))
        }
    }
}

/// Parse `git diff` unified output into a list of [`FileDiff`]s.
///
/// Each file section begins with `diff --git a/... b/...`. We split on that
/// prefix, then extract old/new paths from the `---`/`+++` headers and treat
/// everything after the first `@@` line as the patch text.
fn parse_unified_diff(diff_bytes: &[u8]) -> Vec<FileDiff> {
    use crate::types::Change;

    let text = String::from_utf8_lossy(diff_bytes);
    let mut results = Vec::new();

    // Split into per-file sections on the "diff --git" header.
    for section in text.split("\ndiff --git ") {
        let section = if let Some(stripped) = section.strip_prefix("diff --git ") {
            stripped
        } else {
            section
        };
        if section.is_empty() {
            continue;
        }

        let mut old_path: Option<PathBuf> = None;
        let mut new_path: Option<PathBuf> = None;
        let mut patch_start = 0usize;

        for (i, line) in section.lines().enumerate() {
            if let Some(p) = line.strip_prefix("--- ") {
                old_path = if p == "/dev/null" {
                    None
                } else {
                    Some(PathBuf::from(p.trim_start_matches("a/")))
                };
            } else if let Some(p) = line.strip_prefix("+++ ") {
                new_path = if p == "/dev/null" {
                    None
                } else {
                    Some(PathBuf::from(p.trim_start_matches("b/")))
                };
            } else if line.starts_with("@@") {
                // Everything from this line onward is the patch.
                patch_start = section.lines().take(i).map(|l| l.len() + 1).sum::<usize>();
                break;
            }
        }

        let patch = section[patch_start..].to_string();

        let change = match (&old_path, &new_path) {
            (None, Some(_)) => Change::Added,
            (Some(_), None) => Change::Deleted,
            _ => Change::Modified,
        };

        results.push(FileDiff {
            old_path,
            new_path,
            change,
            patch,
        });
    }

    results
}

fn read_diff_path(repo: &gix::Repository, path: &Path) -> Result<Diff> {
    let dir = workdir(repo)?;
    // HEAD vs working tree for the given path.
    let out = git_cmd(&dir, &["diff", "HEAD", "--", path.to_str().unwrap_or("")]);
    if !out.status.success() {
        return Err(Error::Diff(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    let mut diffs = parse_unified_diff(&out.stdout);
    Ok(diffs.pop().unwrap_or(FileDiff {
        old_path: Some(path.to_path_buf()),
        new_path: Some(path.to_path_buf()),
        change: crate::types::Change::Modified,
        patch: String::new(),
    }))
}

fn read_diff_index(repo: &gix::Repository) -> Result<Vec<FileDiff>> {
    let dir = workdir(repo)?;
    let out = git_cmd(&dir, &["diff", "--cached"]);
    if !out.status.success() {
        return Err(Error::Diff(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(parse_unified_diff(&out.stdout))
}

fn read_diff_worktree(repo: &gix::Repository) -> Result<Vec<FileDiff>> {
    let dir = workdir(repo)?;
    // Unstaged changes (index vs working tree).
    let out = git_cmd(&dir, &["diff"]);
    if !out.status.success() {
        return Err(Error::Diff(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    Ok(parse_unified_diff(&out.stdout))
}

fn read_log(repo: &gix::Repository, _path: Option<&Path>, limit: usize) -> Result<Vec<Commit>> {
    let head_id = repo
        .head_id()
        .map_err(|e| Error::Log(format!("resolving HEAD: {e}")))?;

    let walk = head_id
        .ancestors()
        .all()
        .map_err(|e| Error::Log(format!("walking ancestors: {e}")))?;

    let mut out = Vec::new();
    for info in walk.take(limit) {
        let info = info.map_err(|e| Error::Log(format!("walk entry: {e}")))?;
        let commit = repo
            .find_object(info.id)
            .map_err(|e| Error::Log(format!("loading commit {}: {e}", info.id)))?
            .try_into_commit()
            .map_err(|e| Error::Log(format!("not a commit: {e}")))?;

        let decoded = commit
            .decode()
            .map_err(|e| Error::Log(format!("decoding commit: {e}")))?;
        // decoded.author is &BStr (raw header bytes); use .author() to parse.
        let author = decoded
            .author()
            .map_err(|e| Error::Log(format!("parsing author: {e}")))?;
        let author_time = author.time().map(|t| t.seconds).unwrap_or(0);

        out.push(Commit {
            sha: Sha::from_hex(info.id.to_string()),
            author_name: author.name.to_string(),
            author_email: author.email.to_string(),
            author_time,
            message: decoded.message.to_string(),
        });
    }
    // _path filter is a follow-up — gix's path-restricted log requires a
    // diff-walk and isn't yet wired here.
    Ok(out)
}

fn read_rev_parse(repo: &gix::Repository, ref_name: &str) -> Result<Sha> {
    let id = repo
        .rev_parse_single(ref_name)
        .map_err(|e| Error::RevParse {
            ref_name: ref_name.to_string(),
            message: e.to_string(),
        })?;
    Ok(Sha::from_hex(id.to_string()))
}

fn read_ls_tree(repo: &gix::Repository, sha: &str) -> Result<Vec<TreeEntry>> {
    use crate::types::ObjectKind;

    let oid = repo.rev_parse_single(sha).map_err(|e| Error::Object {
        sha: sha.to_string(),
        message: e.to_string(),
    })?;
    let tree = repo
        .find_object(oid)
        .map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("find_object: {e}"),
        })?
        .peel_to_tree()
        .map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("peel_to_tree: {e}"),
        })?;

    let mut out = Vec::new();
    for entry in tree.iter() {
        let entry = entry.map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("tree entry: {e}"),
        })?;
        let mode = entry.mode();
        let kind = if mode.is_tree() {
            ObjectKind::Tree
        } else if mode.is_commit() {
            ObjectKind::Commit
        } else {
            ObjectKind::Blob
        };
        out.push(TreeEntry {
            name: entry.filename().to_string(),
            sha: Sha::from_hex(entry.oid().to_string()),
            mode: mode.value() as u32,
            kind,
        });
    }
    Ok(out)
}

fn read_show(repo: &gix::Repository, sha: &str, path: &Path) -> Result<Vec<u8>> {
    let commit_id = repo.rev_parse_single(sha).map_err(|e| Error::Object {
        sha: sha.to_string(),
        message: format!("rev_parse: {e}"),
    })?;
    let commit = repo
        .find_object(commit_id)
        .map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("find_object: {e}"),
        })?
        .try_into_commit()
        .map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("not a commit: {e}"),
        })?;

    let tree = commit.tree().map_err(|e| Error::Object {
        sha: sha.to_string(),
        message: format!("tree: {e}"),
    })?;

    let entry = tree
        .lookup_entry_by_path(path)
        .map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("lookup_entry_by_path: {e}"),
        })?
        .ok_or_else(|| Error::Object {
            sha: sha.to_string(),
            message: format!("path {} not found in tree", path.display()),
        })?;

    let blob = repo
        .find_object(entry.object_id())
        .map_err(|e| Error::Object {
            sha: sha.to_string(),
            message: format!("loading blob: {e}"),
        })?;

    Ok(blob.data.clone())
}

/// Build the full commit message from `message` + provenance trailers.
///
/// Trailers are appended as a block separated by a blank line, per
/// `docs/design/01-agents-and-council.md` §Action-log level:
///
/// ```text
/// <original message>
///
/// Co-authored-by: Alice <alice@example.com>
/// engram-actions: summarize
/// ```
pub fn build_commit_message(message: &str, opts: &CommitOpts) -> String {
    if opts.co_authors.is_empty() && opts.footer_lines.is_empty() {
        return message.to_string();
    }

    let mut out = message.trim_end().to_string();
    out.push_str("\n\n");
    for name in &opts.co_authors {
        out.push_str(&format!("Co-authored-by: {name}\n"));
    }
    for line in &opts.footer_lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_add(repo: &gix::Repository, paths: &[&Path]) -> Result<()> {
    // gix index-manipulation is not yet stable as a high-level API; subprocess.
    // Track: https://github.com/Byron/gitoxide/issues/301 (gix-index write path)
    let dir = workdir(repo)?;
    let mut args = vec!["add", "--"];
    let path_strs: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    for s in &path_strs {
        args.push(s.as_str());
    }
    git_cmd_success(&dir, &args, |e| {
        Error::Commit(format!("git add failed: {e}"))
    })
}

fn write_restore(repo: &gix::Repository, paths: &[&Path]) -> Result<()> {
    // gix worktree-restore is not yet exposed at a high level; subprocess.
    let dir = workdir(repo)?;
    let mut args = vec!["restore", "--"];
    let path_strs: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    for s in &path_strs {
        args.push(s.as_str());
    }
    git_cmd_success(&dir, &args, |e| {
        Error::Commit(format!("git restore failed: {e}"))
    })
}

fn write_commit(repo: &gix::Repository, message: &str, opts: CommitOpts) -> Result<Sha> {
    // gix commit creation requires building a tree from the current index and
    // calling repo.commit_as() — the signature resolution path is more complex
    // than it appears. Using subprocess so we inherit the user's git config
    // (gpg signing, committer identity, hooks) transparently. Trailers are
    // built in Rust before hand-off.
    let dir = workdir(repo)?;
    let full_message = build_commit_message(message, &opts);

    let out = git_cmd(&dir, &["commit", "-m", &full_message]);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Commit(format!(
            "git commit failed ({}): {stderr}",
            out.status
        )));
    }

    // Return the new HEAD SHA.
    let head = repo
        .head_id()
        .map_err(|e| Error::Commit(format!("resolving HEAD after commit: {e}")))?;
    Ok(Sha::from_hex(head.to_string()))
}

fn write_push(repo: &gix::Repository, remote: &str, branch: &str) -> Result<()> {
    // gix push via gix-transport is available in 0.83 but the high-level
    // `repo.push()` surface is not yet stable. Subprocess for now.
    // Track: https://github.com/Byron/gitoxide/issues/703 (push progress)
    let dir = workdir(repo)?;
    git_cmd_success(&dir, &["push", remote, branch], |e| {
        Error::Commit(format!("git push failed: {e}"))
    })
}

fn write_pull(repo: &gix::Repository, remote: &str, branch: &str) -> Result<()> {
    // gix fetch + fast-forward merge is available but requires assembling
    // several steps (fetch, ref resolution, ff-merge). Subprocess is a
    // one-liner that also covers hooks and merge-message conventions.
    let dir = workdir(repo)?;
    git_cmd_success(&dir, &["pull", "--ff-only", remote, branch], |e| {
        Error::Commit(format!("git pull failed: {e}"))
    })
}

// ─── ReadOnlyGit impls ─────────────────────────────────────────────────────

impl ReadOnlyGit for ReadHandle {
    fn status(&self) -> Result<Status> {
        read_status(&self.inner.to_thread_local())
    }
    fn diff(&self, path: &Path) -> Result<Diff> {
        read_diff_path(&self.inner.to_thread_local(), path)
    }
    fn diff_index(&self) -> Result<Vec<FileDiff>> {
        read_diff_index(&self.inner.to_thread_local())
    }
    fn diff_worktree(&self) -> Result<Vec<FileDiff>> {
        read_diff_worktree(&self.inner.to_thread_local())
    }
    fn log(&self, path: Option<&Path>, limit: usize) -> Result<Vec<Commit>> {
        read_log(&self.inner.to_thread_local(), path, limit)
    }
    fn show(&self, sha: &str, path: &Path) -> Result<Vec<u8>> {
        read_show(&self.inner.to_thread_local(), sha, path)
    }
    fn ls_tree(&self, sha: &str) -> Result<Vec<TreeEntry>> {
        read_ls_tree(&self.inner.to_thread_local(), sha)
    }
    fn rev_parse(&self, ref_name: &str) -> Result<Sha> {
        read_rev_parse(&self.inner.to_thread_local(), ref_name)
    }
}

impl ReadOnlyGit for WriteHandle {
    fn status(&self) -> Result<Status> {
        read_status(&self.inner.to_thread_local())
    }
    fn diff(&self, path: &Path) -> Result<Diff> {
        read_diff_path(&self.inner.to_thread_local(), path)
    }
    fn diff_index(&self) -> Result<Vec<FileDiff>> {
        read_diff_index(&self.inner.to_thread_local())
    }
    fn diff_worktree(&self) -> Result<Vec<FileDiff>> {
        read_diff_worktree(&self.inner.to_thread_local())
    }
    fn log(&self, path: Option<&Path>, limit: usize) -> Result<Vec<Commit>> {
        read_log(&self.inner.to_thread_local(), path, limit)
    }
    fn show(&self, sha: &str, path: &Path) -> Result<Vec<u8>> {
        read_show(&self.inner.to_thread_local(), sha, path)
    }
    fn ls_tree(&self, sha: &str) -> Result<Vec<TreeEntry>> {
        read_ls_tree(&self.inner.to_thread_local(), sha)
    }
    fn rev_parse(&self, ref_name: &str) -> Result<Sha> {
        read_rev_parse(&self.inner.to_thread_local(), ref_name)
    }
}

// ─── WriteGit impl (write methods only; reads come from ReadOnlyGit) ───────

impl WriteGit for WriteHandle {
    fn add(&self, paths: &[&Path]) -> Result<()> {
        write_add(&self.inner.to_thread_local(), paths)
    }
    fn restore(&self, paths: &[&Path]) -> Result<()> {
        write_restore(&self.inner.to_thread_local(), paths)
    }
    fn commit(&self, message: &str, opts: CommitOpts) -> Result<Sha> {
        write_commit(&self.inner.to_thread_local(), message, opts)
    }
    fn push(&self, remote: &str, branch: &str) -> Result<()> {
        write_push(&self.inner.to_thread_local(), remote, branch)
    }
    fn pull(&self, remote: &str, branch: &str) -> Result<()> {
        write_pull(&self.inner.to_thread_local(), remote, branch)
    }
}
