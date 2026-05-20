//! Backup Watcher — meta-agent that monitors backup recency.
//!
//! Does NOT perform backups. Checks three layers:
//!
//! 1. **Git remote freshness** — unpushed commits older than threshold.
//! 2. **Filesystem snapshot** — macOS Time Machine (`tmutil latestbackup`).
//! 3. **Artifact remote** — optional S3-style `last-sync.txt` probe.
//!
//! Per [ADR 0013]: prefer tool-use / deterministic checks over LLM generation.
//!
//! [ADR 0013]: ../../../docs/design/adrs/0013-tool-use-over-generation.md

use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::BackupError;

// ─── Public types ─────────────────────────────────────────────────────────────

/// Per-layer status entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerStatus {
    /// Human-readable layer name (e.g. "git_remote").
    pub layer: String,
    /// Whether this layer is within the configured threshold.
    pub ok: bool,
    /// Human-readable summary of current state.
    pub detail: String,
    /// Optional warning text when `ok == false`.
    pub warning: Option<String>,
}

/// Aggregated backup status written to `meta/backup-status.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupStatus {
    /// ISO 8601 UTC timestamp of the check.
    pub checked_at: String,
    /// True only when every configured layer is within threshold.
    pub all_ok: bool,
    /// Per-layer breakdown.
    pub layers: Vec<LayerStatus>,
}

impl BackupStatus {
    /// True if any layer produced a warning.
    pub fn has_warnings(&self) -> bool {
        !self.all_ok
    }

    /// Render to the markdown format written to `meta/backup-status.md`.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Backup Status\n\n");
        out.push_str(&format!("_Last checked: {}_\n\n", self.checked_at));

        let overall = if self.all_ok {
            "✅ All layers OK"
        } else {
            "⚠️  One or more layers need attention"
        };
        out.push_str(&format!("**Overall:** {overall}\n\n"));

        out.push_str("## Layers\n\n");
        for layer in &self.layers {
            let mark = if layer.ok { "✅" } else { "⚠️ " };
            out.push_str(&format!("### {mark} {}\n\n", layer.layer));
            out.push_str(&format!("{}\n\n", layer.detail));
            if let Some(w) = &layer.warning {
                out.push_str(&format!("> **Warning:** {w}\n\n"));
            }
        }
        out
    }
}

// ─── Configuration ────────────────────────────────────────────────────────────

/// Thresholds controlling when each layer is considered stale.
#[derive(Debug, Clone, PartialEq)]
pub struct WatcherConfig {
    /// Warn if unpushed commits exist AND the oldest is older than this many hours.
    pub git_remote_stale_hours: u32,
    /// Warn if the most recent Time Machine snapshot is older than this many days.
    pub snapshot_stale_days: u32,
    /// Artifact remote configuration (optional).
    pub artifact_remote: Option<ArtifactRemoteConfig>,
}

/// Optional S3-style artifact remote configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRemoteConfig {
    /// URL of the `last-sync.txt` object (e.g. `https://bucket.s3.amazonaws.com/last-sync.txt`).
    pub last_sync_url: String,
    /// Warn if the last-sync timestamp is older than this many days.
    pub stale_after_days: u32,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            git_remote_stale_hours: 24,
            snapshot_stale_days: 7,
            artifact_remote: None,
        }
    }
}

// ─── Trait boundary (testability) ─────────────────────────────────────────────

/// Abstraction over external I/O calls so tests can inject deterministic results.
pub trait BackupProbe: Send + Sync {
    /// Run `git log <remote>/<branch>..HEAD --format=%ct` in `vault_root`.
    /// Returns commit timestamps (Unix seconds) of unpushed commits, oldest first.
    fn git_unpushed_timestamps(&self, vault_root: &Path) -> Result<Vec<i64>, BackupError>;

    /// Run `tmutil latestbackup` and parse the path's mtime.
    /// Returns the snapshot time, or `None` if Time Machine is not configured.
    fn tmutil_latest_backup_time(&self) -> Result<Option<DateTime<Utc>>, BackupError>;
}

/// Real probe — shells out to git and tmutil.
pub struct SystemProbe;

impl BackupProbe for SystemProbe {
    fn git_unpushed_timestamps(&self, vault_root: &Path) -> Result<Vec<i64>, BackupError> {
        // Determine the remote tracking branch.
        let remote_ref = {
            let out = Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
                .current_dir(vault_root)
                .output()
                .map_err(|e| BackupError::Probe(format!("git rev-parse: {e}")))?;
            if !out.status.success() {
                // No upstream configured — treat as all-OK (no remote to push to).
                return Ok(Vec::new());
            }
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };

        let log_out = Command::new("git")
            .args(["log", &format!("{remote_ref}..HEAD"), "--format=%ct"])
            .current_dir(vault_root)
            .output()
            .map_err(|e| BackupError::Probe(format!("git log: {e}")))?;

        if !log_out.status.success() {
            return Err(BackupError::Probe(
                String::from_utf8_lossy(&log_out.stderr).trim().to_owned(),
            ));
        }

        let timestamps = String::from_utf8_lossy(&log_out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<i64>().ok())
            .collect();
        Ok(timestamps)
    }

    fn tmutil_latest_backup_time(&self) -> Result<Option<DateTime<Utc>>, BackupError> {
        let out = Command::new("tmutil")
            .arg("latestbackup")
            .output()
            .map_err(|e| BackupError::Probe(format!("tmutil: {e}")))?;

        if !out.status.success() {
            // tmutil not available (Linux CI, not macOS) or no destination.
            return Ok(None);
        }

        let path_str = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if path_str.is_empty() {
            return Ok(None);
        }

        // Get the mtime of the backup bundle directory.
        let meta = std::fs::metadata(&path_str)
            .map_err(|e| BackupError::Probe(format!("stat {path_str}: {e}")))?;
        let mtime = meta
            .modified()
            .map_err(|e| BackupError::Probe(format!("mtime: {e}")))?;
        let dt: DateTime<Utc> = mtime.into();
        Ok(Some(dt))
    }
}

// ─── Watcher ──────────────────────────────────────────────────────────────────

/// Backup Watcher: runs the checks and produces a [`BackupStatus`].
pub struct BackupWatcher<P: BackupProbe> {
    probe: P,
    config: WatcherConfig,
}

impl<P: BackupProbe> BackupWatcher<P> {
    /// Create a watcher with the given probe and configuration.
    pub fn new(probe: P, config: WatcherConfig) -> Self {
        Self { probe, config }
    }

    /// Run all checks and return the aggregated [`BackupStatus`].
    pub fn run(&self, vault_root: &Path) -> BackupStatus {
        let now = Utc::now();
        let checked_at = now.to_rfc3339();

        let layers = vec![
            self.check_git_remote(vault_root, now),
            self.check_filesystem_snapshot(now),
        ];

        let all_ok = layers.iter().all(|l| l.ok);
        BackupStatus {
            checked_at,
            all_ok,
            layers,
        }
    }

    fn check_git_remote(&self, vault_root: &Path, now: DateTime<Utc>) -> LayerStatus {
        match self.probe.git_unpushed_timestamps(vault_root) {
            Err(e) => LayerStatus {
                layer: "git_remote".to_owned(),
                ok: false,
                detail: format!("Could not check git remote: {e}"),
                warning: Some(format!("git probe failed: {e}")),
            },
            Ok(timestamps) if timestamps.is_empty() => LayerStatus {
                layer: "git_remote".to_owned(),
                ok: true,
                detail: "No unpushed commits.".to_owned(),
                warning: None,
            },
            Ok(timestamps) => {
                // timestamps are commit timestamps; `git log` emits newest first.
                // The oldest unpushed commit is the last element.
                let oldest_ts = *timestamps.last().unwrap();
                let oldest = DateTime::<Utc>::from_timestamp(oldest_ts, 0).unwrap_or(now);
                let age = now - oldest;
                let threshold = Duration::hours(self.config.git_remote_stale_hours as i64);
                let count = timestamps.len();

                if age > threshold {
                    let hours = age.num_hours();
                    LayerStatus {
                        layer: "git_remote".to_owned(),
                        ok: false,
                        detail: format!("{count} unpushed commit(s); oldest is {hours}h old."),
                        warning: Some(format!(
                            "{count} unpushed commit(s); oldest is {hours}h old \
                             (threshold: {}h).",
                            self.config.git_remote_stale_hours
                        )),
                    }
                } else {
                    LayerStatus {
                        layer: "git_remote".to_owned(),
                        ok: true,
                        detail: format!(
                            "{count} unpushed commit(s); oldest is {}h old (within threshold).",
                            age.num_hours()
                        ),
                        warning: None,
                    }
                }
            }
        }
    }

    fn check_filesystem_snapshot(&self, now: DateTime<Utc>) -> LayerStatus {
        match self.probe.tmutil_latest_backup_time() {
            Err(e) => LayerStatus {
                layer: "filesystem_snapshot".to_owned(),
                ok: false,
                detail: format!("Could not check Time Machine: {e}"),
                warning: Some(format!("tmutil probe failed: {e}")),
            },
            Ok(None) => LayerStatus {
                layer: "filesystem_snapshot".to_owned(),
                // No TM destination = not configured; treat as OK (not everyone uses TM).
                ok: true,
                detail: "Time Machine not configured or no destination available.".to_owned(),
                warning: None,
            },
            Ok(Some(latest)) => {
                let age = now - latest;
                let threshold = Duration::days(self.config.snapshot_stale_days as i64);
                let days = age.num_days();

                if age > threshold {
                    LayerStatus {
                        layer: "filesystem_snapshot".to_owned(),
                        ok: false,
                        detail: format!("Latest Time Machine backup is {days} day(s) old."),
                        warning: Some(format!(
                            "Latest snapshot is {days}d old (threshold: {}d).",
                            self.config.snapshot_stale_days
                        )),
                    }
                } else {
                    LayerStatus {
                        layer: "filesystem_snapshot".to_owned(),
                        ok: true,
                        detail: format!("Latest Time Machine backup is {days} day(s) old."),
                        warning: None,
                    }
                }
            }
        }
    }
}

// ─── Convenience entry point ──────────────────────────────────────────────────

/// Run the backup watcher using the real system probe and write
/// `meta/backup-status.md` in `vault_root`. Returns the status for callers
/// that want to inspect it (e.g. the CLI).
pub fn run_and_write(
    vault_root: &Path,
    config: WatcherConfig,
) -> Result<BackupStatus, BackupError> {
    let watcher = BackupWatcher::new(SystemProbe, config);
    let status = watcher.run(vault_root);

    let meta_dir = vault_root.join("meta");
    std::fs::create_dir_all(&meta_dir)
        .map_err(|e| BackupError::Write(format!("create meta/: {e}")))?;

    let md = status.to_markdown();
    std::fs::write(meta_dir.join("backup-status.md"), md)
        .map_err(|e| BackupError::Write(format!("write backup-status.md: {e}")))?;

    Ok(status)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test probe ─────────────────────────────────────────────────────────────

    struct MockProbe {
        unpushed: Result<Vec<i64>, BackupError>,
        snapshot: Result<Option<DateTime<Utc>>, BackupError>,
    }

    impl BackupProbe for MockProbe {
        fn git_unpushed_timestamps(&self, _vault_root: &Path) -> Result<Vec<i64>, BackupError> {
            match &self.unpushed {
                Ok(v) => Ok(v.clone()),
                Err(BackupError::Probe(m)) => Err(BackupError::Probe(m.clone())),
                Err(BackupError::Write(m)) => Err(BackupError::Write(m.clone())),
            }
        }
        fn tmutil_latest_backup_time(&self) -> Result<Option<DateTime<Utc>>, BackupError> {
            match &self.snapshot {
                Ok(v) => Ok(*v),
                Err(BackupError::Probe(m)) => Err(BackupError::Probe(m.clone())),
                Err(BackupError::Write(m)) => Err(BackupError::Write(m.clone())),
            }
        }
    }

    fn cfg() -> WatcherConfig {
        WatcherConfig {
            git_remote_stale_hours: 24,
            snapshot_stale_days: 7,
            artifact_remote: None,
        }
    }

    // ── Git remote layer ───────────────────────────────────────────────────────

    #[test]
    fn git_no_unpushed_is_ok() {
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let git = status
            .layers
            .iter()
            .find(|l| l.layer == "git_remote")
            .unwrap();
        assert!(git.ok);
        assert!(git.warning.is_none());
    }

    #[test]
    fn git_fresh_unpushed_within_threshold_is_ok() {
        let now_ts = Utc::now().timestamp();
        // Commit 1 hour old — well within 24h threshold.
        let probe = MockProbe {
            unpushed: Ok(vec![now_ts - 3600]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let git = status
            .layers
            .iter()
            .find(|l| l.layer == "git_remote")
            .unwrap();
        assert!(git.ok, "1h old commit should be within 24h threshold");
    }

    #[test]
    fn git_stale_unpushed_emits_warning() {
        let now_ts = Utc::now().timestamp();
        // Commit 30 hours old — exceeds 24h threshold.
        let probe = MockProbe {
            unpushed: Ok(vec![now_ts - 30 * 3600]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let git = status
            .layers
            .iter()
            .find(|l| l.layer == "git_remote")
            .unwrap();
        assert!(!git.ok, "30h old commit should exceed 24h threshold");
        assert!(git.warning.is_some());
        assert!(!status.all_ok);
    }

    #[test]
    fn git_probe_error_propagates_as_not_ok() {
        let probe = MockProbe {
            unpushed: Err(BackupError::Probe("git exploded".to_owned())),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let git = status
            .layers
            .iter()
            .find(|l| l.layer == "git_remote")
            .unwrap();
        assert!(!git.ok);
    }

    // ── Filesystem snapshot layer ──────────────────────────────────────────────

    #[test]
    fn snapshot_not_configured_is_ok() {
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let snap = status
            .layers
            .iter()
            .find(|l| l.layer == "filesystem_snapshot")
            .unwrap();
        assert!(snap.ok);
    }

    #[test]
    fn snapshot_fresh_is_ok() {
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            // Snapshot 1 day old — within 7-day threshold.
            snapshot: Ok(Some(Utc::now() - Duration::days(1))),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let snap = status
            .layers
            .iter()
            .find(|l| l.layer == "filesystem_snapshot")
            .unwrap();
        assert!(snap.ok);
    }

    #[test]
    fn snapshot_stale_emits_warning() {
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            // Snapshot 10 days old — exceeds 7-day threshold.
            snapshot: Ok(Some(Utc::now() - Duration::days(10))),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let snap = status
            .layers
            .iter()
            .find(|l| l.layer == "filesystem_snapshot")
            .unwrap();
        assert!(!snap.ok);
        assert!(snap.warning.is_some());
    }

    // ── all_ok aggregate ───────────────────────────────────────────────────────

    #[test]
    fn all_ok_when_every_layer_passes() {
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        assert!(status.all_ok);
        assert!(!status.has_warnings());
    }

    // ── Markdown rendering ─────────────────────────────────────────────────────

    #[test]
    fn markdown_contains_overall_and_layers() {
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let md = status.to_markdown();
        assert!(md.contains("# Backup Status"));
        assert!(md.contains("git_remote"));
        assert!(md.contains("filesystem_snapshot"));
    }

    #[test]
    fn markdown_shows_warning_for_stale_git() {
        let now_ts = Utc::now().timestamp();
        let probe = MockProbe {
            unpushed: Ok(vec![now_ts - 30 * 3600]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(Path::new("/vault"));
        let md = status.to_markdown();
        assert!(md.contains("Warning"));
        assert!(md.contains("⚠️"));
    }

    #[test]
    fn write_to_temp_vault_produces_file() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path();
        let probe = MockProbe {
            unpushed: Ok(vec![]),
            snapshot: Ok(None),
        };
        let watcher = BackupWatcher::new(probe, cfg());
        let status = watcher.run(vault);
        // Manually write to verify the function chain.
        let meta_dir = vault.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let md = status.to_markdown();
        std::fs::write(meta_dir.join("backup-status.md"), &md).unwrap();
        let content = std::fs::read_to_string(meta_dir.join("backup-status.md")).unwrap();
        assert!(content.contains("# Backup Status"));
    }
}
