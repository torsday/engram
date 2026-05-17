//! The read/write trait split that enforces ADR 0003 ("agents never commit")
//! at the type system level — see ADR 0009.
//!
//! [`ReadOnlyGit`] is the only handle agent code ever sees. [`WriteGit`]
//! extends it with mutation methods; it is constructed exactly once at process
//! startup by the API/CLI layers and is unreachable from agent code by
//! construction.
//!
//! The compile-fail test under `tests/compile_fail/` pins this invariant: any
//! attempt to call [`WriteGit::commit`] through a `&dyn ReadOnlyGit` fails to
//! compile with an error indicating the method is not on the read-only trait.

use std::path::Path;

use crate::error::Result;
use crate::types::{Commit, CommitOpts, Diff, FileDiff, Sha, Status, TreeEntry};

/// Read-only git operations. Available to all agents.
///
/// `Send + Sync` so handles can be cloned across threads and tasks. The
/// concrete handle ([`crate::ReadHandle`]) holds a `gix::ThreadSafeRepository`
/// internally and produces a thread-local `gix::Repository` per call.
pub trait ReadOnlyGit: Send + Sync {
    /// Working-tree status. Equivalent to `git status` with both staged and
    /// unstaged entries.
    fn status(&self) -> Result<Status>;

    /// Diff for a single path — HEAD vs working tree, unified format.
    fn diff(&self, path: &Path) -> Result<Diff>;

    /// Diff of the index against HEAD (staged but not yet committed).
    fn diff_index(&self) -> Result<Vec<FileDiff>>;

    /// Diff of the working tree against the index (unstaged).
    fn diff_worktree(&self) -> Result<Vec<FileDiff>>;

    /// Commit history. If `path` is `Some`, restrict to commits that touched
    /// that path. `limit` caps the number of returned commits.
    fn log(&self, path: Option<&Path>, limit: usize) -> Result<Vec<Commit>>;

    /// Read the blob at `path` as it existed at commit `sha`.
    fn show(&self, sha: &str, path: &Path) -> Result<Vec<u8>>;

    /// List the entries of the tree object identified by `sha`.
    fn ls_tree(&self, sha: &str) -> Result<Vec<TreeEntry>>;

    /// Resolve a ref name (branch, tag, short SHA, `HEAD~3`, etc.) to a full
    /// 40-char SHA.
    fn rev_parse(&self, ref_name: &str) -> Result<Sha>;
}

/// Mutating git operations. NOT available to agents — only HTTP handlers and
/// CLI subcommands invoked by the human ever receive a [`WriteGit`] value.
///
/// The trait extends [`ReadOnlyGit`] so a write handle can also read, but the
/// concrete [`crate::WriteHandle`] and [`crate::ReadHandle`] are **distinct
/// nominal types**: a `ReadHandle` cannot be upcast to a `WriteHandle` in safe
/// Rust. See `crate::repo::open` for the constructor.
pub trait WriteGit: ReadOnlyGit {
    /// Stage `paths` for the next commit — `git add <paths>`.
    fn add(&self, paths: &[&Path]) -> Result<()>;

    /// Discard worktree or index changes for `paths` — `git restore <paths>`.
    fn restore(&self, paths: &[&Path]) -> Result<()>;

    /// Create a commit from the current index with `message`. `opts` controls
    /// co-author and footer trailers appended to the message.
    fn commit(&self, message: &str, opts: CommitOpts) -> Result<Sha>;

    /// Push `branch` to `remote`.
    fn push(&self, remote: &str, branch: &str) -> Result<()>;

    /// Pull `branch` from `remote`.
    fn pull(&self, remote: &str, branch: &str) -> Result<()>;
}
