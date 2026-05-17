//! gix-backed git operations. Agents may read history and write unstaged files only.
//! No agent ever calls `git add` or `git commit` — that boundary is enforced here.
//! See ADR 0003 (no-agent-commits) and ADR 0009 (git-read-write-boundary).

/// Read-only history inspection: log, diff, blame, revision walk.
pub mod history {}

/// Unstaged write primitives: write file to working tree without staging.
pub mod write {}

/// Diff generation for the review queue (unstaged changes → structured diff).
pub mod diff {}

/// Provenance metadata embedded in commit messages by the human committer.
pub mod provenance {}
