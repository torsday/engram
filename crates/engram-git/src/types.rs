//! Value types returned by the [`crate::ReadOnlyGit`] and [`crate::WriteGit`]
//! traits. These are intentionally backend-agnostic — they expose only what
//! engram's review queue, agent runner, and HTTP handlers need, not the full
//! richness of gix's domain model.

use std::path::PathBuf;

/// A 40-character lowercase SHA-1 git object id.
///
/// Newtype prevents mixing object ids with arbitrary strings at call sites.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha(String);

impl Sha {
    /// Construct from an existing hex string. The caller is responsible for
    /// having validated the format — typically obtained from gix.
    pub fn from_hex(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Sha {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-file change classification surfaced by status and diff queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

/// Working-tree status entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: PathBuf,
    pub change: Change,
    /// Whether the change is staged (in the index) vs only in the worktree.
    pub staged: bool,
}

/// Aggregated status of the working tree.
///
/// `clean()` is true when no staged, unstaged, or untracked changes exist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub entries: Vec<StatusEntry>,
}

impl Status {
    pub fn clean(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Per-file diff entry — both for single-path diffs and bulk listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub change: Change,
    /// Unified diff text. Empty when the change is "added with no content" or
    /// "deleted with no prior content" (binary or empty file).
    pub patch: String,
}

/// Single-path diff. Aliased to [`FileDiff`] — the ADR distinguishes the names
/// at the trait surface but the data shape is identical.
pub type Diff = FileDiff;

/// Commit metadata returned by [`crate::ReadOnlyGit::log`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: Sha,
    pub author_name: String,
    pub author_email: String,
    /// Seconds since the Unix epoch (author time).
    pub author_time: i64,
    /// Full commit message (subject + body).
    pub message: String,
}

/// Object kind for tree entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Blob,
    Tree,
    Commit,
    Tag,
}

/// Single entry in a tree object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub sha: Sha,
    /// Git mode (e.g. 0o100644 for a regular file).
    pub mode: u32,
    pub kind: ObjectKind,
}

/// Options for [`crate::WriteGit::commit`].
///
/// `co_authors` and `footer_lines` are appended to the commit message so the
/// engram pre-commit hook can record `engram-actions: ...` trailers — see
/// `docs/design/01-agents-and-council.md` §Action-log level.
#[derive(Debug, Clone, Default)]
pub struct CommitOpts {
    pub co_authors: Vec<String>,
    pub footer_lines: Vec<String>,
}
