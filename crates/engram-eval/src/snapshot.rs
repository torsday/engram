//! Vault snapshot unpacker for the eval framework.
//!
//! Each [`crate::Case`] points at a `vault_state` snapshot — the
//! pre-seeded vault the runner unpacks before invoking the agent.
//! This module turns that pointer into a real directory tree under
//! a destination of the caller's choice (typically a `tempdir`).
//!
//! # Snapshot kinds
//!
//! This slice supports three source forms:
//!
//! - **Directory**: a vault tree on disk. `unpack_snapshot` does a
//!   recursive copy under `dest`. Useful for hand-curated case
//!   fixtures during early development.
//! - **Tarball** (`.tar`): stream-extracted into `dest`.
//! - **Gzipped tarball** (`.tar.gz` / `.tgz`): `GzDecoder` + tar
//!   streaming extraction. Expected shipping format for most case
//!   fixtures — vault snapshots compress well.
//!
//! Out of scope for this slice (separate follow-ups):
//!
//! - Content-addressed snapshot cache at
//!   `.engram/evals/snapshots/<sha>/` (the spec's preferred layout)
//! - `.zip` / `.tar.zst` / other compressed formats
//! - Permission/ACL preservation beyond what `std::fs::copy` does

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors from [`unpack_snapshot`].
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// The snapshot source path doesn't exist (or isn't readable).
    #[error("snapshot source {path} not found or unreadable")]
    SourceNotFound {
        /// Source path the caller supplied.
        path: PathBuf,
    },

    /// The snapshot source is a file format this slice doesn't
    /// handle yet (e.g. `.tar`, `.tar.gz`). A follow-up will add
    /// extraction support; surfacing this loudly today avoids
    /// silent "vault is empty" failures during the seed step.
    #[error("snapshot kind for {path} is not supported (yet): {hint}")]
    UnsupportedKind {
        /// Source path the caller supplied.
        path: PathBuf,
        /// Human-readable hint about what's missing
        /// (e.g. `"tar extraction is a follow-up slice"`).
        hint: &'static str,
    },

    /// A filesystem operation during copy failed.
    #[error("snapshot unpack failed at {path}: {source}")]
    Io {
        /// Path being read or written when the failure occurred.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Unpack the snapshot at `src` into `dest`. The `dest` directory
/// is created (with parents) if missing; existing contents are
/// **not** cleared — the caller picks a fresh location (typically a
/// `tempdir`) for each case run.
///
/// Currently supports directory sources only; `.tar` / compressed
/// formats return [`SnapshotError::UnsupportedKind`].
pub fn unpack_snapshot(src: &Path, dest: &Path) -> Result<(), SnapshotError> {
    let meta = std::fs::metadata(src).map_err(|_| SnapshotError::SourceNotFound {
        path: src.to_path_buf(),
    })?;

    if meta.is_file() {
        // `.tar.gz` has a compound extension; only the trailing `.gz`
        // surfaces via `Path::extension`. Match on both the trailing
        // extension and the file_name to distinguish `.tgz` and the
        // common `.tar.gz` two-part form.
        let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
        return match ext {
            "tar" => unpack_tar(src, dest),
            "tgz" => unpack_tar_gz(src, dest),
            "gz" if name.ends_with(".tar.gz") => unpack_tar_gz(src, dest),
            "gz" => Err(SnapshotError::UnsupportedKind {
                path: src.to_path_buf(),
                hint: "plain-gzip (non-tar) extraction is not supported",
            }),
            "zip" => Err(SnapshotError::UnsupportedKind {
                path: src.to_path_buf(),
                hint: "zip extraction is a follow-up slice",
            }),
            _ => Err(SnapshotError::UnsupportedKind {
                path: src.to_path_buf(),
                hint: "snapshot file must be a .tar / .tar.gz / .tgz or a directory",
            }),
        };
    }

    if !meta.is_dir() {
        // Symlink / fifo / unknown — out of scope.
        return Err(SnapshotError::UnsupportedKind {
            path: src.to_path_buf(),
            hint: "snapshot source must be a directory",
        });
    }

    std::fs::create_dir_all(dest).map_err(|e| SnapshotError::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;
    copy_dir_recursive(src, dest)
}

/// Extract a `.tar` archive into `dest`. Wraps the `tar` crate's
/// streaming reader so memory use stays O(largest entry) regardless
/// of archive size. Permissions are NOT preserved (`set_preserve_permissions(false)`)
/// because case fixtures shouldn't carry executable bits from the
/// originating filesystem into the eval-runner's temp directory.
fn unpack_tar(src: &Path, dest: &Path) -> Result<(), SnapshotError> {
    let file = std::fs::File::open(src).map_err(|e| SnapshotError::Io {
        path: src.to_path_buf(),
        source: e,
    })?;
    std::fs::create_dir_all(dest).map_err(|e| SnapshotError::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;
    let mut archive = tar::Archive::new(file);
    archive.set_preserve_permissions(false);
    archive.set_overwrite(true);
    archive.unpack(dest).map_err(|e| SnapshotError::Io {
        path: src.to_path_buf(),
        source: e,
    })
}

/// Extract a `.tar.gz` / `.tgz` archive into `dest`. Wraps the
/// `tar` reader with `flate2::read::GzDecoder` so memory use stays
/// O(largest entry) regardless of compressed or uncompressed size.
fn unpack_tar_gz(src: &Path, dest: &Path) -> Result<(), SnapshotError> {
    let file = std::fs::File::open(src).map_err(|e| SnapshotError::Io {
        path: src.to_path_buf(),
        source: e,
    })?;
    std::fs::create_dir_all(dest).map_err(|e| SnapshotError::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(false);
    archive.set_overwrite(true);
    archive.unpack(dest).map_err(|e| SnapshotError::Io {
        path: src.to_path_buf(),
        source: e,
    })
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), SnapshotError> {
    let entries = std::fs::read_dir(src).map_err(|e| SnapshotError::Io {
        path: src.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| SnapshotError::Io {
            path: src.to_path_buf(),
            source: e,
        })?;
        let entry_path = entry.path();
        let file_name = entry.file_name();
        let dest_path = dest.join(&file_name);

        let ft = entry.file_type().map_err(|e| SnapshotError::Io {
            path: entry_path.clone(),
            source: e,
        })?;
        if ft.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(|e| SnapshotError::Io {
                path: dest_path.clone(),
                source: e,
            })?;
            copy_dir_recursive(&entry_path, &dest_path)?;
        } else if ft.is_file() {
            std::fs::copy(&entry_path, &dest_path).map_err(|e| SnapshotError::Io {
                path: dest_path.clone(),
                source: e,
            })?;
        }
        // Symlinks: skipped silently. The spec doesn't require
        // symlink preservation in case fixtures, and following
        // them blindly invites loops / escape-the-snapshot issues.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn unpack_copies_flat_directory_contents() {
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("a.md"), "alpha").unwrap();
        std::fs::write(src.path().join("b.md"), "beta").unwrap();

        let dest = tempdir().unwrap();
        unpack_snapshot(src.path(), dest.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest.path().join("a.md")).unwrap(),
            "alpha"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("b.md")).unwrap(),
            "beta"
        );
    }

    #[test]
    fn unpack_recurses_into_nested_subdirectories() {
        let src = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("notes/sub")).unwrap();
        std::fs::write(src.path().join("notes/sub/deep.md"), "deep!").unwrap();

        let dest = tempdir().unwrap();
        unpack_snapshot(src.path(), dest.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dest.path().join("notes/sub/deep.md")).unwrap(),
            "deep!"
        );
    }

    #[test]
    fn unpack_creates_missing_dest_directory() {
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("only.md"), "x").unwrap();

        let parent = tempdir().unwrap();
        let dest = parent.path().join("created/by/unpack");
        assert!(!dest.exists());
        unpack_snapshot(src.path(), &dest).unwrap();
        assert!(dest.join("only.md").exists());
    }

    #[test]
    fn missing_source_returns_source_not_found() {
        let parent = tempdir().unwrap();
        let missing = parent.path().join("not-there");
        match unpack_snapshot(&missing, parent.path()) {
            Err(SnapshotError::SourceNotFound { path }) => assert_eq!(path, missing),
            other => panic!("expected SourceNotFound, got {other:?}"),
        }
    }

    /// A real `.tar` source unpacks into `dest` with file contents
    /// preserved across nested paths.
    #[test]
    fn tar_source_extracts_contents_into_dest() {
        // Build a tar from a real directory so the archive header
        // is well-formed.
        let src = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("notes")).unwrap();
        std::fs::write(src.path().join("notes/a.md"), "alpha-body").unwrap();
        std::fs::write(src.path().join("top.md"), "top-body").unwrap();

        let parent = tempdir().unwrap();
        let tar_path = parent.path().join("vault.tar");
        {
            let file = std::fs::File::create(&tar_path).unwrap();
            let mut builder = tar::Builder::new(file);
            builder.append_dir_all(".", src.path()).unwrap();
            builder.finish().unwrap();
        }

        let dest = tempdir().unwrap();
        unpack_snapshot(&tar_path, dest.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.path().join("notes/a.md")).unwrap(),
            "alpha-body"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("top.md")).unwrap(),
            "top-body"
        );
    }

    /// A truncated / malformed `.tar` surfaces the underlying tar
    /// error as an `Io` variant (not silently succeeding).
    #[test]
    fn malformed_tar_source_surfaces_io_error() {
        let parent = tempdir().unwrap();
        let tar_path = parent.path().join("vault.tar");
        std::fs::write(&tar_path, b"not-a-real-tar-header").unwrap();
        let dest = tempdir().unwrap();
        match unpack_snapshot(&tar_path, dest.path()) {
            Err(SnapshotError::Io { path, .. }) => {
                // Source path is what we tried to read, not the
                // dest, when the archive itself is the problem.
                assert_eq!(path, tar_path);
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }

    /// Build a tar from a source dir then gzip-compress the result
    /// in-memory, write it as `<name>.tar.gz`, and verify
    /// unpack_snapshot extracts it correctly.
    #[test]
    fn tar_gz_source_extracts_contents_into_dest() {
        // Build a directory to archive.
        let src = tempdir().unwrap();
        std::fs::create_dir_all(src.path().join("notes")).unwrap();
        std::fs::write(src.path().join("notes/a.md"), "alpha-gz").unwrap();
        std::fs::write(src.path().join("top.md"), "top-gz").unwrap();

        // Compose the .tar.gz: tar::Builder writing into GzEncoder.
        let parent = tempdir().unwrap();
        let gz_path = parent.path().join("vault.tar.gz");
        {
            let file = std::fs::File::create(&gz_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            builder.append_dir_all(".", src.path()).unwrap();
            // Drop the builder so the GzEncoder finishes the stream.
            builder.into_inner().unwrap().finish().unwrap();
        }

        let dest = tempdir().unwrap();
        unpack_snapshot(&gz_path, dest.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.path().join("notes/a.md")).unwrap(),
            "alpha-gz"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("top.md")).unwrap(),
            "top-gz"
        );
    }

    /// `.tgz` extension routes through the same gz path.
    #[test]
    fn tgz_extension_routes_through_gz_path() {
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("only.md"), "tgz!").unwrap();

        let parent = tempdir().unwrap();
        let tgz_path = parent.path().join("vault.tgz");
        {
            let file = std::fs::File::create(&tgz_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            builder.append_dir_all(".", src.path()).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let dest = tempdir().unwrap();
        unpack_snapshot(&tgz_path, dest.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.path().join("only.md")).unwrap(),
            "tgz!"
        );
    }

    /// A plain `.gz` (non-tar) source is still rejected — only
    /// `.tar.gz` / `.tgz` get extracted.
    #[test]
    fn plain_gz_source_returns_unsupported_with_distinct_hint() {
        let parent = tempdir().unwrap();
        let gz_path = parent.path().join("vault.gz");
        std::fs::write(&gz_path, b"\0\0\0\0").unwrap();
        match unpack_snapshot(&gz_path, parent.path()) {
            Err(SnapshotError::UnsupportedKind { hint, .. }) => {
                assert!(hint.contains("plain-gzip"));
            }
            other => panic!("expected UnsupportedKind, got {other:?}"),
        }
    }

    #[test]
    fn empty_source_directory_unpacks_to_empty_destination() {
        let src = tempdir().unwrap();
        let dest = tempdir().unwrap();
        unpack_snapshot(src.path(), dest.path()).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dest.path()).unwrap().collect();
        assert!(entries.is_empty());
    }
}
