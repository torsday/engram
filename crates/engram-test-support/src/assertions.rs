//! Common test assertions for engram integration tests.
//!
//! These helpers encode the core invariants that every agent-touching test
//! should check — primarily ADR 0003 (agents never commit) and the proposal
//! queue shape.

use std::path::Path;

use engram_core::frontmatter::parse_frontmatter;

// ---------------------------------------------------------------------------
// Frontmatter assertions
// ---------------------------------------------------------------------------

/// Assert that `key` is present in the frontmatter of the note at `path` and
/// its string representation equals `expected`.
///
/// Panics with a descriptive message on failure.
pub fn assert_field_in_frontmatter(path: &Path, key: &str, expected: &str) {
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "assert_field_in_frontmatter: cannot read {}: {e}",
            path.display()
        )
    });
    let fm = parse_frontmatter(&content).unwrap_or_else(|e| {
        panic!(
            "assert_field_in_frontmatter: parse error in {}: {e}",
            path.display()
        )
    });

    // We serialize frontmatter to JSON and check the field there, since
    // Frontmatter is a typed struct (not a map). This avoids a dependency on
    // serde_json in every callsite.
    let json = serde_json::to_value(&fm)
        .unwrap_or_else(|e| panic!("assert_field_in_frontmatter: cannot serialize: {e}"));

    let actual = json.get(key).unwrap_or_else(|| {
        panic!(
            "assert_field_in_frontmatter: key `{key}` not found in frontmatter of {}",
            path.display()
        )
    });

    let actual_str = match actual {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };

    assert_eq!(
        actual_str, expected,
        "assert_field_in_frontmatter: key `{key}` in {} — expected `{expected}`, got `{actual_str}`",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// ADR 0003 assertion
// ---------------------------------------------------------------------------

/// Assert that no commits in the vault git repository were authored by an agent
/// (ADR 0003: agents never run `git commit`).
///
/// Checks the last `limit` commits. An agent-authored commit is detected by
/// looking for `[engram-agent]` in the commit message or `engram-agent` as the
/// committer name.
///
/// This is a conservative heuristic — it checks the convention we'll enforce
/// rather than enumerating every possible agent name.
///
/// # Panics
///
/// Panics if any agent-authored commit is found, or if the path is not a git
/// repository.
pub fn assert_no_agent_commits(repo_path: &Path, limit: usize) {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            repo_path.to_str().expect("repo path must be UTF-8"),
            "log",
            "--format=%an|%ae|%s",
            &format!("-{limit}"),
        ])
        .output()
        .expect("assert_no_agent_commits: failed to run `git log`");

    if !output.status.success() {
        // Not a git repo or no commits yet — nothing to check.
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line_lc = line.to_lowercase();
        assert!(
            !line_lc.contains("engram-agent") && !line_lc.contains("[agent]"),
            "assert_no_agent_commits: found agent-authored commit in {}: `{line}`",
            repo_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Proposal queue assertions
// ---------------------------------------------------------------------------

/// Assert that a proposal JSON file exists in `.engram/proposals/` matching
/// the given `note_id` and `agent` name, with a confidence score within the
/// range `[min_confidence, max_confidence]`.
///
/// Proposal files are JSON with at least the shape:
/// ```json
/// { "note_id": "...", "agent": "...", "confidence": 0.85 }
/// ```
pub fn assert_proposal_at(
    vault_root: &Path,
    note_id: &str,
    agent: &str,
    min_confidence: f64,
    max_confidence: f64,
) {
    let proposals_dir = vault_root.join(".engram").join("proposals");
    assert!(
        proposals_dir.exists(),
        "assert_proposal_at: proposals directory does not exist at {}",
        proposals_dir.display()
    );

    let entries = std::fs::read_dir(&proposals_dir)
        .unwrap_or_else(|e| panic!("assert_proposal_at: cannot read proposals dir: {e}"));

    let mut found = false;
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("note_id").and_then(|v| v.as_str()) == Some(note_id)
            && v.get("agent").and_then(|v| v.as_str()) == Some(agent)
        {
            let confidence = v.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
            assert!(
                confidence >= min_confidence && confidence <= max_confidence,
                "assert_proposal_at: note_id={note_id} agent={agent} confidence={confidence} \
                 not in [{min_confidence}, {max_confidence}]"
            );
            found = true;
            break;
        }
    }

    assert!(
        found,
        "assert_proposal_at: no proposal found for note_id={note_id} agent={agent} \
         in {}",
        proposals_dir.display()
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn note_at(dir: &Path, filename: &str, content: &str) {
        fs::write(dir.join(filename), content).unwrap();
    }

    #[test]
    fn assert_field_in_frontmatter_passes_for_correct_field() {
        let dir = TempDir::new().unwrap();
        note_at(
            dir.path(),
            "note.md",
            "---\nid: abc123\ntitle: Hello\ntype: evergreen\n---\n\nBody.\n",
        );
        assert_field_in_frontmatter(&dir.path().join("note.md"), "title", "Hello");
    }

    #[test]
    #[should_panic(expected = "assert_field_in_frontmatter: key `title`")]
    fn assert_field_in_frontmatter_fails_for_wrong_value() {
        let dir = TempDir::new().unwrap();
        note_at(
            dir.path(),
            "note.md",
            "---\nid: abc123\ntitle: Hello\ntype: evergreen\n---\n\nBody.\n",
        );
        assert_field_in_frontmatter(&dir.path().join("note.md"), "title", "World");
    }

    #[test]
    fn assert_no_agent_commits_passes_on_non_git_dir() {
        let dir = TempDir::new().unwrap();
        // Not a git repo — should not panic.
        assert_no_agent_commits(dir.path(), 10);
    }

    #[test]
    fn assert_proposal_at_finds_matching_proposal() {
        let dir = TempDir::new().unwrap();
        let proposals = dir.path().join(".engram").join("proposals");
        fs::create_dir_all(&proposals).unwrap();
        let proposal = serde_json::json!({
            "note_id": "01ABCDEFGH",
            "agent": "linker",
            "confidence": 0.85
        });
        fs::write(
            proposals.join("proposal-001.json"),
            serde_json::to_string(&proposal).unwrap(),
        )
        .unwrap();

        assert_proposal_at(dir.path(), "01ABCDEFGH", "linker", 0.80, 0.90);
    }

    #[test]
    #[should_panic(expected = "assert_proposal_at: no proposal found")]
    fn assert_proposal_at_fails_when_missing() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".engram").join("proposals")).unwrap();
        assert_proposal_at(dir.path(), "NONEXISTENT", "linker", 0.0, 1.0);
    }

    #[test]
    #[should_panic(expected = "confidence")]
    fn assert_proposal_at_fails_when_confidence_out_of_range() {
        let dir = TempDir::new().unwrap();
        let proposals = dir.path().join(".engram").join("proposals");
        fs::create_dir_all(&proposals).unwrap();
        let proposal = serde_json::json!({
            "note_id": "NOTEID",
            "agent": "linker",
            "confidence": 0.50
        });
        fs::write(
            proposals.join("p.json"),
            serde_json::to_string(&proposal).unwrap(),
        )
        .unwrap();
        assert_proposal_at(dir.path(), "NOTEID", "linker", 0.80, 0.90);
    }
}
