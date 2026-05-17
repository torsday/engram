//! Agent code calling `commit` on a `ReadOnlyGit` handle MUST fail to
//! compile — the type system is the enforcement mechanism for ADR 0003.

use engram_git::{CommitOpts, ReadOnlyGit};

fn agent_does_a_bad_thing(git: &dyn ReadOnlyGit) {
    let _ = git.commit("agents should not be able to do this", CommitOpts::default());
}

fn main() {}
