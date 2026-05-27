//! Integration test: every workspace-level
//! `tests/fixtures/agents/<name>/output/*.json` fixture must
//! validate cleanly against its agent's typed Rust output schema
//! via the `validate()` dispatch in
//! `engram_agents::agents::validate`.
//!
//! Per `docs/design/12-agent-spec-template.md` step 5 ("Add test
//! fixtures at `tests/fixtures/agents/<name>/`"), each agent
//! carries an exemplar happy-path output. This test locks the
//! fixture + the typed Rust struct + the prompt's documented JSON
//! schema together in one round-trip path:
//!
//! - If the prompt schema changes (slice 1), the fixture must
//!   change too (this slice) and the typed struct must change
//!   too (slice 2). A drift between any two surfaces here.
//! - The drift surfaces specifically through the `validate()`
//!   dispatch from PR #289, so a missing `validate` arm for a
//!   new agent is also caught.

use engram_agents::agents::validate::{validate, ValidationError};
use std::path::PathBuf;

/// The workspace root, computed from the test binary's manifest
/// dir. Walk up from `crates/engram-agents/` to the workspace
/// (two levels).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root reachable from manifest dir")
        .to_path_buf()
}

/// Walk `tests/fixtures/agents/` and for each `<name>/output/*.json`
/// fixture, run `validate(name, contents)`. Each missing/unparseable
/// fixture panics with the agent name + reason so test output
/// points directly at the offending file.
#[test]
fn every_agent_happy_path_fixture_validates() {
    let fixtures_root = workspace_root().join("tests/fixtures/agents");
    assert!(
        fixtures_root.is_dir(),
        "expected fixtures root at {} — Doc 12 step 5",
        fixtures_root.display()
    );

    let mut checked = 0usize;
    for agent_dir in std::fs::read_dir(&fixtures_root).expect("read fixtures dir") {
        let agent_dir = agent_dir.expect("dirent");
        if !agent_dir.file_type().unwrap().is_dir() {
            continue;
        }
        let agent_name = agent_dir
            .file_name()
            .to_str()
            .expect("agent dirs are utf-8")
            .to_string();
        let output_dir = agent_dir.path().join("output");
        if !output_dir.is_dir() {
            continue;
        }

        let mut per_agent = 0usize;
        for fixture in std::fs::read_dir(&output_dir).expect("read output dir") {
            let fixture = fixture.expect("dirent");
            let path = fixture.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
            match validate(&agent_name, &raw) {
                Ok(()) => {}
                Err(ValidationError::UnknownAgent { name }) => panic!(
                    "fixture at {} targets agent `{}` which has no registered \
                     typed-output validator — add the agent to \
                     `engram_agents::agents::validate` or move the fixture",
                    path.display(),
                    name
                ),
                Err(ValidationError::ParseFailed { name, source }) => panic!(
                    "fixture {} failed validate({}, ...): {}\n\
                     This usually means the prompt schema changed but the \
                     typed Rust struct or this fixture didn't. Bring them \
                     back in lockstep.",
                    path.display(),
                    name,
                    source
                ),
            }
            per_agent += 1;
            checked += 1;
        }

        assert!(
            per_agent >= 1,
            "agent `{agent_name}` has a fixture directory but no .json files; \
             Doc 12 step 5 requires at least one happy-path fixture per agent"
        );
    }

    // The on-disk agent floor is 9 (asserted in
    // `runner::tests::on_disk_agent_files_parse`). Every agent
    // now has a happy-path fixture and an alternate-shape
    // fixture (decline / alternate-mode / end) — 18 total — so
    // the floor here moves in lockstep. A missing fixture for
    // any agent surfaces drift between slice 1 (files on disk)
    // and slice 5 (test fixtures, per Doc 12).
    assert!(
        checked >= 18,
        "expected at least two fixtures per agent (happy + alternate; 18 total); \
         checked {checked}"
    );
}
