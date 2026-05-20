//! Vault file watcher.
//!
//! Wraps [`notify`] in an async-friendly interface that:
//!
//! - Watches the vault root (recursive), the agents directory, and
//!   `.git/index` from a single watcher instance;
//! - **Debounces** events per path so a single editor save (which the OS
//!   reports as many `Modify` events) becomes one `WatchEvent`;
//! - **Detects renames** by surviving frontmatter `id:` — when a file
//!   disappears and another appears with the same id inside the rename
//!   window, the pair is emitted as a single [`WatchEvent::NoteRenamed`]
//!   instead of `NoteRemoved` + `NoteAdded`;
//! - **Classifies paths** into vault notes (`.md` under `vault_root`),
//!   agent prompt / config edits (`agents/<name>/{prompt.md,config.toml}`),
//!   and git-index changes (`.git/index`);
//! - **Backpressures** via a bounded `tokio::sync::mpsc` channel — when the
//!   consumer falls behind we drop events with a `tracing::warn!` rather
//!   than letting the watcher OOM.
//!
//! Per [ADR 0006](../../../docs/design/adrs/0006-pure-title-slug-filenames.md),
//! the frontmatter `id:` is the canonical identity of a note; surviving the
//! id across renames is how downstream consumers (indexer, agent runner,
//! reconciliation) keep their state attached to the *note*, not the
//! filename.
//!
//! Per [ADR 0009](../../../docs/design/adrs/0009-git-read-write-boundary.md),
//! the watcher never invokes `git add` / `git commit` — it only *observes*
//! `.git/index` to surface a `GitIndexChanged` signal for the agent-actions
//! reconciliation layer.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use engram_core::watcher::{Watcher, WatcherConfig};
//! # async fn ex() -> Result<(), engram_core::watcher::WatcherError> {
//! let (_watcher, mut events) = Watcher::new(
//!     Path::new("/vault"),
//!     Path::new("/vault/agents"),
//!     Path::new("/vault/.git"),
//!     WatcherConfig::default(),
//! )?;
//!
//! while let Some(event) = events.recv().await {
//!     println!("{event:?}");
//! }
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::frontmatter::parse_frontmatter;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from constructing or driving the [`Watcher`].
#[derive(Debug, Error)]
pub enum WatcherError {
    /// The underlying [`notify`] watcher could not be constructed or could
    /// not start observing one of the requested paths.
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),
}

// ---------------------------------------------------------------------------
// WatchEvent
// ---------------------------------------------------------------------------

/// A single, debounced and classified filesystem event emitted by the
/// [`Watcher`].
///
/// `path` is always the absolute on-disk path. `id`, when present, is the
/// note's frontmatter `id:` — surfaced opportunistically so consumers don't
/// have to re-read the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// A note (`.md` file under `vault_root`) was added.
    NoteAdded {
        /// Absolute path to the new note.
        path: PathBuf,
        /// `id:` from the note's frontmatter, if it parsed successfully.
        id: Option<String>,
    },
    /// An existing note's contents changed.
    NoteModified {
        /// Absolute path to the note.
        path: PathBuf,
        /// `id:` from the note's frontmatter, if it parsed successfully.
        id: Option<String>,
    },
    /// A note was removed. `id` is the last id we saw for this path (if any).
    NoteRemoved {
        /// Absolute path that disappeared.
        path: PathBuf,
        /// Last-known `id:` from before the file disappeared.
        id: Option<String>,
    },
    /// A note was renamed — detected because the `id:` from a recently
    /// removed file matched the `id:` of a newly created one inside the
    /// rename window.
    NoteRenamed {
        /// Path the note used to live at.
        from: PathBuf,
        /// Path it now lives at.
        to: PathBuf,
        /// `id:` that survived the rename.
        id: String,
    },
    /// An agent's `config.toml` was added, modified, or removed.
    AgentConfigChanged {
        /// Agent directory name (the `<name>` in `agents/<name>/config.toml`).
        agent_name: String,
    },
    /// An agent's `prompt.md` was added, modified, or removed.
    AgentPromptChanged {
        /// Agent directory name (the `<name>` in `agents/<name>/prompt.md`).
        agent_name: String,
    },
    /// The git index changed (e.g. a `git add`).
    GitIndexChanged,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Tunable parameters for the [`Watcher`].
///
/// Defaults match `docs/design/03-architecture.md` §Data flow: 2 s debounce,
/// 500 ms rename window, 256-event bounded queue.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// How long a path must be quiet before the most recent event for it is
    /// emitted. Coalesces rapid editor saves into a single event.
    pub debounce: Duration,
    /// Time window for matching a `NoteRemoved`-then-`NoteAdded` pair into
    /// a single `NoteRenamed` via surviving frontmatter `id:`.
    pub rename_window: Duration,
    /// Bounded capacity of the outbound `mpsc` channel. When the consumer
    /// is slower than the producer, events past the cap are dropped with a
    /// `tracing::warn!` rather than OOM-ing the watcher.
    pub channel_capacity: usize,
    /// How often the debounce dispatcher wakes to flush quiet events.
    /// Lower = lower per-event latency, higher CPU. Default `100 ms`.
    pub tick_interval: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_secs(2),
            rename_window: Duration::from_millis(500),
            channel_capacity: 256,
            tick_interval: Duration::from_millis(100),
        }
    }
}

// ---------------------------------------------------------------------------
// Watcher
// ---------------------------------------------------------------------------

/// Handle to a running watcher. Dropping it stops the underlying `notify`
/// watcher and the dispatcher task; the receiver returned alongside the
/// handle yields no further events.
pub struct Watcher {
    // Field order matters for Drop: the dispatcher task must finish before
    // the notify watcher is dropped, because the dispatcher reads from a
    // channel the watcher's callback owns. Rust drops fields in source
    // order; we abort the dispatcher *first* (manual `abort()` on Drop),
    // then let `_native` fall out of scope.
    _native: RecommendedWatcher,
    dispatcher: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        if let Some(handle) = self.dispatcher.take() {
            handle.abort();
        }
    }
}

impl Watcher {
    /// Construct a watcher and start observing `vault_root`, `agents_dir`,
    /// and `git_dir/index`.
    ///
    /// Returns the [`Watcher`] handle (drop to stop) plus an
    /// `mpsc::Receiver` on which events arrive.
    ///
    /// The watcher must be constructed within a running tokio runtime —
    /// the dispatcher is a `tokio::spawn`ed task. `notify`'s underlying
    /// platform thread is independent.
    pub fn new(
        vault_root: &Path,
        agents_dir: &Path,
        git_dir: &Path,
        config: WatcherConfig,
    ) -> Result<(Self, mpsc::Receiver<WatchEvent>), WatcherError> {
        // Canonicalize so that path classification works on platforms where
        // notify reports symlink-resolved paths (e.g. macOS tmpdirs live at
        // `/var/...` but FSEvents reports `/private/var/...`).
        let vault_root = canonicalize_for_watch(vault_root)?;
        let agents_dir = canonicalize_for_watch(agents_dir)?;
        let git_dir = canonicalize_for_watch(git_dir)?;

        let (out_tx, out_rx) = mpsc::channel::<WatchEvent>(config.channel_capacity);
        let (raw_tx, raw_rx) = mpsc::unbounded_channel::<notify::Event>();

        // notify's callback runs on its platform thread; we forward events
        // to the async dispatcher via an unbounded tokio channel. The
        // unbounded channel is safe because the dispatcher drains it
        // continuously and bounds itself downstream via `out_tx`.
        let mut native: RecommendedWatcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    // Send is fallible only when the receiver has been
                    // dropped (Watcher dropped, dispatcher aborted). That's
                    // expected and silent.
                    let _ = raw_tx.send(ev);
                }
                Err(e) => tracing::warn!(error = %e, "engram-core watcher: notify reported error"),
            })?;

        native.watch(&vault_root, RecursiveMode::Recursive)?;
        // agents_dir is allowed to overlap vault_root; notify handles
        // duplicate registration by emitting events through both watches,
        // and our path classifier dedupes by sending the most-specific
        // classification first.
        if agents_dir != vault_root && !agents_dir.starts_with(&vault_root) {
            native.watch(&agents_dir, RecursiveMode::Recursive)?;
        }
        let index_path = git_dir.join("index");
        // Watch the *directory* containing `.git/index` non-recursively
        // because notify on some platforms doesn't fire events for a
        // single-file watch when the file is replaced (git rewrites the
        // index by atomic-rename). Only watch separately if it isn't
        // already covered by the recursive vault watch.
        if !git_dir.starts_with(&vault_root) {
            native.watch(&git_dir, RecursiveMode::NonRecursive)?;
        }

        let ctx = DispatchContext {
            vault_root,
            agents_dir,
            git_dir,
            index_path,
            config,
        };

        let dispatcher = tokio::spawn(dispatch_loop(raw_rx, out_tx, ctx));

        Ok((
            Self {
                _native: native,
                dispatcher: Some(dispatcher),
            },
            out_rx,
        ))
    }
}

/// Resolve a path through any symlinks before storing it for comparison
/// against notify-reported paths.
///
/// notify on macOS reports paths from FSEvents that have been canonicalized
/// (e.g. `/var/folders/.../tmp` → `/private/var/folders/.../tmp`); on Linux
/// inotify paths come through as-given. Canonicalizing at startup lets the
/// classifier's `starts_with` comparisons work on both platforms with a
/// single code path.
fn canonicalize_for_watch(p: &Path) -> Result<PathBuf, WatcherError> {
    p.canonicalize().map_err(|e| {
        WatcherError::Notify(notify::Error::io(std::io::Error::new(
            e.kind(),
            format!("canonicalize {:?}: {}", p, e),
        )))
    })
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct DispatchContext {
    vault_root: PathBuf,
    agents_dir: PathBuf,
    git_dir: PathBuf,
    index_path: PathBuf,
    config: WatcherConfig,
}

/// A pending event for a path, awaiting debounce settling. The exact
/// note-event kind (Added vs Modified vs Removed) is determined at flush
/// time from filesystem state — see [`emit_for_note`] — because macOS
/// FSEvents reports `Modify(Metadata)` for many transitions that Linux
/// inotify reports as distinct Create/Remove events. Filesystem state at
/// flush time is the only portable signal.
#[derive(Debug)]
struct Pending {
    region: PendingRegion,
    last_seen: Instant,
}

#[derive(Debug, Clone)]
enum PendingRegion {
    Note,
    AgentConfig { agent_name: String },
    AgentPrompt { agent_name: String },
    GitIndex,
}

/// Map id -> (path, removed_at) for rename detection. Populated on every
/// NoteRemoved emit; consulted on every NoteAdded emit.
type PendingRemoves = HashMap<String, (PathBuf, Instant)>;

async fn dispatch_loop(
    mut raw_rx: mpsc::UnboundedReceiver<notify::Event>,
    out_tx: mpsc::Sender<WatchEvent>,
    ctx: DispatchContext,
) {
    // path -> last-known id (so a Remove can still report the id).
    let mut path_to_id: HashMap<PathBuf, String> = HashMap::new();
    let mut pending: HashMap<PathBuf, Pending> = HashMap::new();
    let mut pending_removes: PendingRemoves = HashMap::new();

    let mut tick = tokio::time::interval(ctx.config.tick_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            maybe_ev = raw_rx.recv() => {
                let Some(ev) = maybe_ev else { break }; // sender dropped
                ingest_raw(&ev, &ctx, &mut pending, &path_to_id);
            }
            _ = tick.tick() => {
                flush_quiet(&ctx, &out_tx, &mut pending, &mut pending_removes, &mut path_to_id).await;
                prune_pending_removes(&mut pending_removes, ctx.config.rename_window);
            }
        }
    }

    // Channel closed — flush whatever is ready, then exit.
    flush_quiet(
        &ctx,
        &out_tx,
        &mut pending,
        &mut pending_removes,
        &mut path_to_id,
    )
    .await;
}

/// Classify a raw notify event into pending entries. Multiple paths in a
/// single notify::Event are processed independently.
fn ingest_raw(
    ev: &notify::Event,
    ctx: &DispatchContext,
    pending: &mut HashMap<PathBuf, Pending>,
    _path_to_id: &HashMap<PathBuf, String>,
) {
    let now = Instant::now();
    for path in &ev.paths {
        let Some(region) = classify(path, ev, ctx) else {
            continue;
        };
        // For modify+name(both) we may see two paths in one event (from, to);
        // we treat both as their own classify pass and let the dispatcher
        // pair them via id-survival.
        pending.insert(
            path.clone(),
            Pending {
                region,
                last_seen: now,
            },
        );
    }
}

/// Path classifier. Returns the *region* the path falls under; the exact
/// Note event kind (Added vs Modified vs Removed) is decided at flush time
/// based on filesystem state, because notify event kinds are platform-
/// inconsistent (FSEvents in particular reports many transitions as
/// `Modify(Metadata)` regardless of whether the file was created or
/// removed).
fn classify(path: &Path, ev: &notify::Event, ctx: &DispatchContext) -> Option<PendingRegion> {
    // .git/index — direct path equality (and ignore the catch-all "Other").
    if path == ctx.index_path
        || (path.starts_with(&ctx.git_dir)
            && path.file_name().and_then(|n| n.to_str()) == Some("index"))
    {
        return matches!(
            ev.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        )
        .then_some(PendingRegion::GitIndex);
    }

    // Agents dir: `agents/<name>/{prompt.md,config.toml}` only.
    if path.starts_with(&ctx.agents_dir) {
        let rel = path.strip_prefix(&ctx.agents_dir).ok()?;
        let mut comps = rel.components();
        let agent_name = comps.next()?.as_os_str().to_string_lossy().into_owned();
        let leaf = comps.next()?.as_os_str().to_string_lossy().into_owned();
        // Reject deeper paths — only the canonical two files matter.
        if comps.next().is_some() {
            return None;
        }
        return match leaf.as_str() {
            "config.toml" => Some(PendingRegion::AgentConfig { agent_name }),
            "prompt.md" => Some(PendingRegion::AgentPrompt { agent_name }),
            _ => None,
        };
    }

    // Vault notes: any `.md` under vault_root, excluding the .git subtree.
    // (The agents subtree is already matched above.)
    if path.starts_with(&ctx.vault_root)
        && !path.starts_with(&ctx.git_dir)
        && path.extension().and_then(|e| e.to_str()) == Some("md")
        && matches!(
            ev.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        )
    {
        return Some(PendingRegion::Note);
    }

    None
}

/// Emit every pending entry whose `last_seen` is older than `debounce`.
///
/// Note-region entries are processed in two passes so that rename pairing
/// works regardless of HashMap iteration order: pass 1 handles paths whose
/// file is gone (which populates `pending_removes` with the surviving id);
/// pass 2 handles paths whose file exists (so the `NoteAdded` branch can
/// consult `pending_removes` and promote to a `NoteRenamed`). Without this
/// split, a fast `rename(from, to)` could be flushed `to`-first and emit
/// `NoteAdded` before the matching `NoteRemoved` had a chance to register
/// its id.
async fn flush_quiet(
    ctx: &DispatchContext,
    out_tx: &mpsc::Sender<WatchEvent>,
    pending: &mut HashMap<PathBuf, Pending>,
    pending_removes: &mut PendingRemoves,
    path_to_id: &mut HashMap<PathBuf, String>,
) {
    let now = Instant::now();
    let ready: Vec<PathBuf> = pending
        .iter()
        .filter(|(_, p)| now.duration_since(p.last_seen) >= ctx.config.debounce)
        .map(|(k, _)| k.clone())
        .collect();

    // Partition into "file is gone" (removes-first pass) and "file exists or
    // non-note region" (rest). Non-note regions are stable regardless of
    // order — we lump them with the second pass for simplicity.
    let (gone_paths, present_paths): (Vec<_>, Vec<_>) = ready.into_iter().partition(|p| {
        matches!(
            pending.get(p),
            Some(Pending {
                region: PendingRegion::Note,
                ..
            })
        ) && !p.exists()
    });

    let emit_path = |path: PathBuf,
                     pending: &mut HashMap<PathBuf, Pending>,
                     pending_removes: &mut PendingRemoves,
                     path_to_id: &mut HashMap<PathBuf, String>| {
        let Some(p) = pending.remove(&path) else {
            return;
        };
        let emit = build_emit(&path, p.region, pending_removes, path_to_id, now);
        if let Some(event) = emit {
            if let Err(e) = out_tx.try_send(event) {
                tracing::warn!(error = %e, "engram-core watcher: outbound channel full; event dropped");
            }
        }
    };

    for path in gone_paths {
        emit_path(path, pending, pending_removes, path_to_id);
    }
    for path in present_paths {
        emit_path(path, pending, pending_removes, path_to_id);
    }
}

/// Decide the exact event kind from filesystem state at flush time and
/// update the `path_to_id` / `pending_removes` caches as a side effect.
///
/// For [`PendingRegion::Note`], the (exists, was_known) pair is the
/// authoritative signal: if the file is gone now we emit `NoteRemoved`; if
/// it's there but we hadn't seen it before, `NoteAdded`; otherwise
/// `NoteModified`. A ghost (gone + never known) is dropped — it's a path
/// that was created and deleted inside a single debounce window with no
/// observable net effect.
fn build_emit(
    path: &Path,
    region: PendingRegion,
    pending_removes: &mut PendingRemoves,
    path_to_id: &mut HashMap<PathBuf, String>,
    now: Instant,
) -> Option<WatchEvent> {
    match region {
        PendingRegion::Note => {
            let exists = path.exists();
            let was_known = path_to_id.contains_key(path);
            match (exists, was_known) {
                (false, false) => None, // ghost: created and deleted within a window
                (false, true) => {
                    let id = path_to_id.remove(path);
                    if let Some(ref id_str) = id {
                        pending_removes.insert(id_str.clone(), (path.to_path_buf(), now));
                    }
                    Some(WatchEvent::NoteRemoved {
                        path: path.to_path_buf(),
                        id,
                    })
                }
                (true, false) => {
                    let id = read_note_id(path);
                    if let Some(ref id_str) = id {
                        if let Some((from, _)) = pending_removes.remove(id_str) {
                            path_to_id.insert(path.to_path_buf(), id_str.clone());
                            return Some(WatchEvent::NoteRenamed {
                                from,
                                to: path.to_path_buf(),
                                id: id_str.clone(),
                            });
                        }
                        path_to_id.insert(path.to_path_buf(), id_str.clone());
                    }
                    Some(WatchEvent::NoteAdded {
                        path: path.to_path_buf(),
                        id,
                    })
                }
                (true, true) => {
                    // Refresh the id cache opportunistically — the note's id
                    // shouldn't change but the cache might be stale on the
                    // first observation of a pre-existing file.
                    let id = read_note_id(path);
                    if let Some(ref id_str) = id {
                        path_to_id.insert(path.to_path_buf(), id_str.clone());
                    }
                    Some(WatchEvent::NoteModified {
                        path: path.to_path_buf(),
                        id,
                    })
                }
            }
        }
        PendingRegion::AgentConfig { agent_name } => {
            Some(WatchEvent::AgentConfigChanged { agent_name })
        }
        PendingRegion::AgentPrompt { agent_name } => {
            Some(WatchEvent::AgentPromptChanged { agent_name })
        }
        PendingRegion::GitIndex => Some(WatchEvent::GitIndexChanged),
    }
}

/// Best-effort read of a note's frontmatter `id:`. Returns `None` if the
/// file is unreadable or the frontmatter is malformed — neither is fatal
/// to the watcher's operation.
fn read_note_id(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_frontmatter(&content).ok().map(|fm| fm.id)
}

/// Drop entries from `pending_removes` whose `removed_at` is older than the
/// rename window — at that point a matching add will no longer be paired.
fn prune_pending_removes(map: &mut PendingRemoves, window: Duration) {
    let now = Instant::now();
    map.retain(|_, (_, at)| now.duration_since(*at) <= window);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_for(vault: &Path) -> DispatchContext {
        DispatchContext {
            vault_root: vault.to_path_buf(),
            agents_dir: vault.join("agents"),
            git_dir: vault.join(".git"),
            index_path: vault.join(".git/index"),
            config: WatcherConfig::default(),
        }
    }

    fn fake_event(kind: EventKind, path: PathBuf) -> notify::Event {
        notify::Event {
            kind,
            paths: vec![path],
            attrs: Default::default(),
        }
    }

    // Pull in ModifyKind for the few classify-tests that exercise it.
    use notify::event::ModifyKind;

    #[test]
    fn classifies_vault_md_as_note_region() {
        let vault = PathBuf::from("/vault");
        let ctx = ctx_for(&vault);
        let p = vault.join("notes/x.md");
        let ev = fake_event(EventKind::Create(notify::event::CreateKind::Any), p.clone());
        assert!(matches!(classify(&p, &ev, &ctx), Some(PendingRegion::Note)));
    }

    #[test]
    fn classifies_non_md_as_none() {
        let vault = PathBuf::from("/vault");
        let ctx = ctx_for(&vault);
        let p = vault.join("notes/cover.png");
        let ev = fake_event(EventKind::Create(notify::event::CreateKind::Any), p.clone());
        assert!(classify(&p, &ev, &ctx).is_none());
    }

    #[test]
    fn classifies_agent_prompt() {
        let vault = PathBuf::from("/vault");
        let ctx = ctx_for(&vault);
        let p = vault.join("agents/curator/prompt.md");
        let ev = fake_event(EventKind::Modify(ModifyKind::Any), p.clone());
        match classify(&p, &ev, &ctx) {
            Some(PendingRegion::AgentPrompt { agent_name }) => assert_eq!(agent_name, "curator"),
            other => panic!("expected AgentPrompt, got {other:?}"),
        }
    }

    #[test]
    fn classifies_agent_config() {
        let vault = PathBuf::from("/vault");
        let ctx = ctx_for(&vault);
        let p = vault.join("agents/linker/config.toml");
        let ev = fake_event(EventKind::Modify(ModifyKind::Any), p.clone());
        match classify(&p, &ev, &ctx) {
            Some(PendingRegion::AgentConfig { agent_name }) => assert_eq!(agent_name, "linker"),
            other => panic!("expected AgentConfig, got {other:?}"),
        }
    }

    #[test]
    fn classifies_agent_other_file_as_none() {
        let vault = PathBuf::from("/vault");
        let ctx = ctx_for(&vault);
        let p = vault.join("agents/curator/README.md");
        let ev = fake_event(EventKind::Modify(ModifyKind::Any), p.clone());
        assert!(classify(&p, &ev, &ctx).is_none());
    }

    #[test]
    fn classifies_git_index() {
        let vault = PathBuf::from("/vault");
        let ctx = ctx_for(&vault);
        let p = ctx.index_path.clone();
        let ev = fake_event(EventKind::Modify(ModifyKind::Any), p.clone());
        assert!(matches!(
            classify(&p, &ev, &ctx),
            Some(PendingRegion::GitIndex)
        ));
    }

    #[test]
    fn prune_drops_stale_pending_removes() {
        let mut map: PendingRemoves = HashMap::new();
        let now = Instant::now();
        map.insert(
            "OLD".into(),
            (PathBuf::from("/old.md"), now - Duration::from_secs(10)),
        );
        map.insert("NEW".into(), (PathBuf::from("/new.md"), now));
        prune_pending_removes(&mut map, Duration::from_millis(500));
        assert!(map.contains_key("NEW"));
        assert!(!map.contains_key("OLD"));
    }
}
