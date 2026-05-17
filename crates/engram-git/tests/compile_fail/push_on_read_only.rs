//! Agent code calling `push` on a `ReadOnlyGit` handle MUST fail to compile.

use engram_git::ReadOnlyGit;

fn agent_does_a_bad_thing(git: &dyn ReadOnlyGit) {
    let _ = git.push("origin", "main");
}

fn main() {}
