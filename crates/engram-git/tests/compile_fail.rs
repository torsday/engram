//! Compile-fail tests pinning the ADR 0009 invariant: write methods must be
//! unrepresentable on a `ReadOnlyGit` handle.
//!
//! Each `.rs` file under `tests/compile_fail/` is compiled in isolation by
//! `trybuild`. Its stderr is compared against the paired `.stderr` file —
//! regenerate with `TRYBUILD=overwrite cargo test --test compile_fail`.

#[test]
fn agent_code_cannot_commit() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/commit_on_read_only.rs");
    t.compile_fail("tests/compile_fail/push_on_read_only.rs");
    t.compile_fail("tests/compile_fail/add_on_read_only.rs");
}
