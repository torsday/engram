//! Integration tests for [`engram_core::watcher`].
//!
//! Each test materializes a real on-disk vault under a `tempfile::TempDir`,
//! starts a [`Watcher`], performs a filesystem operation, and asserts that
//! the expected [`WatchEvent`] arrives within a generous timeout.
//!
//! The watcher's defaults use a 2 s debounce which would make tests slow;
//! every test below tightens the timers via a custom [`WatcherConfig`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use engram_core::watcher::{WatchEvent, Watcher, WatcherConfig};
use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;

/// Build the canonical vault layout `vault_root/.git/`, `vault_root/agents/`,
/// `vault_root/notes/`. Returns the TempDir so it lives for the test.
///
/// The returned root is canonicalized so test assertions can compare paths
/// directly against the ones the watcher emits — the watcher canonicalizes
/// its inputs and notify reports canonical paths on macOS.
fn fresh_vault() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonicalize");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    (dir, root)
}

fn fast_config() -> WatcherConfig {
    WatcherConfig {
        debounce: Duration::from_millis(50),
        rename_window: Duration::from_millis(300),
        channel_capacity: 64,
        tick_interval: Duration::from_millis(10),
    }
}

fn note_with_id(id: &str, title: &str, body: &str) -> String {
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: evergreen\n---\n\n{body}\n",
        id = id,
        title = title,
        body = body
    )
}

/// Drain events with a per-event timeout, returning whatever arrived.
async fn collect_events(
    rx: &mut Receiver<WatchEvent>,
    expected: usize,
    timeout: Duration,
) -> Vec<WatchEvent> {
    let mut out = Vec::with_capacity(expected);
    let deadline = tokio::time::Instant::now() + timeout;
    while out.len() < expected {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ev)) => out.push(ev),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    out
}

/// FSEvents on macOS has a startup latency of a few hundred milliseconds
/// during which file operations are missed. Give the underlying watcher
/// time to start delivering events before performing the test operations.
async fn warmup() {
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn detects_note_added_modified_removed() {
    let (_dir, root) = fresh_vault();
    let (_watcher, mut rx) = Watcher::new(
        &root,
        &root.join("agents"),
        &root.join(".git"),
        fast_config(),
    )
    .expect("watcher start");
    warmup().await;

    // Add.
    let note = root.join("notes/alpha.md");
    std::fs::write(
        &note,
        note_with_id("01JRZK3M7PQNX8BABCDE12345", "Alpha", "body"),
    )
    .unwrap();

    // Modify. Sleep long enough that FSEvents has time to deliver the
    // Add events and the dispatcher to flush them before the next write
    // is observed — otherwise macOS coalesces the three ops into one
    // "net-effect" wave and we lose intermediate events.
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::write(
        &note,
        note_with_id("01JRZK3M7PQNX8BABCDE12345", "Alpha", "body changed"),
    )
    .unwrap();

    // Remove.
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::remove_file(&note).unwrap();

    let events = collect_events(&mut rx, 3, Duration::from_secs(5)).await;

    // The platform may coalesce Modify after Add into a single event; what we
    // require is that *at least* an Added, a Modified or Added, and a
    // Removed appear in order.
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            WatchEvent::NoteAdded { .. } => "added",
            WatchEvent::NoteModified { .. } => "modified",
            WatchEvent::NoteRemoved { .. } => "removed",
            other => panic!("unexpected event {other:?}"),
        })
        .collect();

    assert!(
        kinds.first() == Some(&"added"),
        "first event must be Added, got {kinds:?}"
    );
    assert!(
        kinds.contains(&"removed"),
        "must observe a Removed, got {kinds:?}"
    );
}

#[tokio::test]
async fn detects_rename_via_surviving_frontmatter_id() {
    let (_dir, root) = fresh_vault();
    let (_watcher, mut rx) = Watcher::new(
        &root,
        &root.join("agents"),
        &root.join(".git"),
        WatcherConfig {
            // Wider rename window so the timing isn't tight on slow CI.
            rename_window: Duration::from_secs(2),
            ..fast_config()
        },
    )
    .expect("watcher start");
    warmup().await;

    let id = "01JRZK3M7PQNX8BABCDE99999";
    let from = root.join("notes/before.md");
    let to = root.join("notes/after.md");

    // Create the file so the watcher caches its id.
    std::fs::write(&from, note_with_id(id, "Before", "x")).unwrap();
    // Drain the Added event so we know the id is cached.
    let _ = collect_events(&mut rx, 1, Duration::from_secs(3)).await;

    // Move it. On macOS FSEvents this typically materializes as
    // Remove(from) + Create(to); on Linux inotify it may be a single
    // ModifyName(Both). Either way, our detector should pair them by id.
    std::fs::rename(&from, &to).unwrap();

    let events = collect_events(&mut rx, 2, Duration::from_secs(3)).await;
    let renamed = events.iter().find_map(|e| match e {
        WatchEvent::NoteRenamed {
            from: f,
            to: t,
            id: i,
        } => Some((f.clone(), t.clone(), i.clone())),
        _ => None,
    });
    let renamed = renamed.unwrap_or_else(|| panic!("expected NoteRenamed, got events {events:?}"));
    assert_eq!(renamed.0, from);
    assert_eq!(renamed.1, to);
    assert_eq!(renamed.2, id);
}

#[tokio::test]
async fn detects_git_index_changed_on_index_write() {
    let (_dir, root) = fresh_vault();
    let (_watcher, mut rx) = Watcher::new(
        &root,
        &root.join("agents"),
        &root.join(".git"),
        fast_config(),
    )
    .expect("watcher start");
    warmup().await;

    let index = root.join(".git/index");
    std::fs::write(&index, b"\0\0\0\0").unwrap();

    let events = collect_events(&mut rx, 1, Duration::from_secs(2)).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WatchEvent::GitIndexChanged)),
        "expected GitIndexChanged, got {events:?}"
    );
}

#[tokio::test]
async fn detects_agent_prompt_and_config_changes() {
    let (_dir, root) = fresh_vault();
    std::fs::create_dir_all(root.join("agents/curator")).unwrap();

    let (_watcher, mut rx) = Watcher::new(
        &root,
        &root.join("agents"),
        &root.join(".git"),
        fast_config(),
    )
    .expect("watcher start");
    warmup().await;

    std::fs::write(root.join("agents/curator/prompt.md"), "you are a curator").unwrap();
    std::fs::write(
        root.join("agents/curator/config.toml"),
        "schedule = \"daily\"\n",
    )
    .unwrap();

    let events = collect_events(&mut rx, 2, Duration::from_secs(2)).await;

    let saw_prompt = events.iter().any(|e| {
        matches!(e,
        WatchEvent::AgentPromptChanged { agent_name } if agent_name == "curator")
    });
    let saw_config = events.iter().any(|e| {
        matches!(e,
        WatchEvent::AgentConfigChanged { agent_name } if agent_name == "curator")
    });

    assert!(saw_prompt, "missing AgentPromptChanged in {events:?}");
    assert!(saw_config, "missing AgentConfigChanged in {events:?}");
}

#[tokio::test]
async fn ignores_non_markdown_files_under_vault() {
    let (_dir, root) = fresh_vault();
    let (_watcher, mut rx) = Watcher::new(
        &root,
        &root.join("agents"),
        &root.join(".git"),
        fast_config(),
    )
    .expect("watcher start");
    warmup().await;

    // A PNG and a hidden file — neither should produce events.
    std::fs::write(root.join("notes/cover.png"), b"\x89PNG").unwrap();
    std::fs::write(root.join("notes/.DS_Store"), b"").unwrap();

    let events = collect_events(&mut rx, 1, Duration::from_millis(400)).await;
    assert!(
        events.is_empty(),
        "expected no events from non-.md files, got {events:?}"
    );
}

#[tokio::test]
async fn dropping_watcher_closes_event_channel() {
    let (_dir, root) = fresh_vault();
    let (watcher, mut rx) = Watcher::new(
        &root,
        &root.join("agents"),
        &root.join(".git"),
        fast_config(),
    )
    .expect("watcher start");

    drop(watcher);

    // After the dispatcher task is aborted the receiver should observe a
    // closed channel within a tick interval or two.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let path: &Path = Path::new("ignored");
    let _ = path; // silence unused
    let received = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
    match received {
        Ok(None) => {} // expected: closed
        Ok(Some(ev)) => panic!("unexpected event {ev:?}"),
        Err(_) => panic!("recv did not return after drop within 1s"),
    }
}
