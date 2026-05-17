//! Agent code calling `add` on a `ReadOnlyGit` handle MUST fail to compile.

use engram_git::ReadOnlyGit;
use std::path::Path;

fn agent_does_a_bad_thing(git: &dyn ReadOnlyGit) {
    let p = Path::new("note.md");
    let _ = git.add(&[p]);
}

fn main() {}
