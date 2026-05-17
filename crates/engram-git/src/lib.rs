//! gix-backed git operations for the engram vault.
//!
//! The crate's central invariant — *agents never run `git add` or `git
//! commit`* — is enforced at the type system level by splitting git access
//! across two traits:
//!
//! - [`ReadOnlyGit`] is what every agent receives. It exposes status, diff,
//!   log, and object-inspection operations.
//! - [`WriteGit`] extends [`ReadOnlyGit`] with `add`, `restore`, `commit`,
//!   `push`, and `pull`. It is constructed exactly once at process startup
//!   inside [`open`] and is reachable only by HTTP handlers and CLI
//!   subcommands invoked by the human.
//!
//! See ADR 0003 (no-agent-commits) and ADR 0009 (git-read-write-boundary).
//!
//! # Example
//!
//! ```no_run
//! use engram_git::{open, ReadOnlyGit};
//! # fn ex() -> engram_git::Result<()> {
//! let (_write, read) = open(std::path::Path::new("."))?;
//! let head = read.rev_parse("HEAD")?;
//! println!("HEAD is {head}");
//! # Ok(()) }
//! ```

pub mod error;
pub mod repo;
pub mod traits;
pub mod types;

/// Re-exports for integration tests only — not part of the public API.
#[doc(hidden)]
pub mod testing {
    pub use crate::repo::build_commit_message;
}

pub use error::{Error, Result};
pub use repo::{open, ReadHandle, WriteHandle};
pub use traits::{ReadOnlyGit, WriteGit};
pub use types::{
    Change, Commit, CommitOpts, Diff, FileDiff, ObjectKind, Sha, Status, StatusEntry, TreeEntry,
};
