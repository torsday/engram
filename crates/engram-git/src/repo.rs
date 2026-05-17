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

fn read_status(repo: &gix::Repository) -> Result<Status> {
    use crate::types::StatusEntry;

    // gix-status is not yet stable in 0.67 for the use cases we need; fall back
    // to `git status --porcelain=v1 -z` via the user's git binary. The boundary
    // this crate enforces is at the trait level — the underlying I/O backend
    // is replaceable. Follow-up: track the gix-status migration once the API
    // stabilises (gix-status reached `0.x` only recently).
    let workdir = repo
        .work_dir()
        .ok_or_else(|| Error::Status("repository has no working tree (bare?)".to_string()))?;

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["status", "--porcelain=v1", "-z"])
        .output()
        .map_err(|e| Error::Status(format!("invoking git: {e}")))?;

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
        let author = decoded.author;

        out.push(Commit {
            sha: Sha::from_hex(info.id.to_string()),
            author_name: author.name.to_string(),
            author_email: author.email.to_string(),
            author_time: author.time.seconds,
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
            mode: mode.0 as u32,
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

// ─── ReadOnlyGit impls ─────────────────────────────────────────────────────

impl ReadOnlyGit for ReadHandle {
    fn status(&self) -> Result<Status> {
        read_status(&self.inner.to_thread_local())
    }
    fn diff(&self, _path: &Path) -> Result<Diff> {
        Err(Error::NotYetImplemented {
            method: "ReadOnlyGit::diff",
        })
    }
    fn diff_index(&self) -> Result<Vec<FileDiff>> {
        Err(Error::NotYetImplemented {
            method: "ReadOnlyGit::diff_index",
        })
    }
    fn diff_worktree(&self) -> Result<Vec<FileDiff>> {
        Err(Error::NotYetImplemented {
            method: "ReadOnlyGit::diff_worktree",
        })
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
    fn diff(&self, _path: &Path) -> Result<Diff> {
        Err(Error::NotYetImplemented {
            method: "ReadOnlyGit::diff",
        })
    }
    fn diff_index(&self) -> Result<Vec<FileDiff>> {
        Err(Error::NotYetImplemented {
            method: "ReadOnlyGit::diff_index",
        })
    }
    fn diff_worktree(&self) -> Result<Vec<FileDiff>> {
        Err(Error::NotYetImplemented {
            method: "ReadOnlyGit::diff_worktree",
        })
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
    fn add(&self, _paths: &[&Path]) -> Result<()> {
        Err(Error::NotYetImplemented {
            method: "WriteGit::add",
        })
    }
    fn restore(&self, _paths: &[&Path]) -> Result<()> {
        Err(Error::NotYetImplemented {
            method: "WriteGit::restore",
        })
    }
    fn commit(&self, _message: &str, _opts: CommitOpts) -> Result<Sha> {
        Err(Error::NotYetImplemented {
            method: "WriteGit::commit",
        })
    }
    fn push(&self, _remote: &str, _branch: &str) -> Result<()> {
        Err(Error::NotYetImplemented {
            method: "WriteGit::push",
        })
    }
    fn pull(&self, _remote: &str, _branch: &str) -> Result<()> {
        Err(Error::NotYetImplemented {
            method: "WriteGit::pull",
        })
    }
}
