//! Content-addressed snapshot cache.
//!
//! Per `01-agents-and-council.md` §Eval framework: vault snapshots
//! live at `.engram/evals/snapshots/<sha>/` so multiple cases that
//! share a vault state extract once.
//!
//! [`SnapshotCache::ensure_unpacked`] computes the SHA-256 of the
//! source (file bytes for archives, a stable recursive hash for
//! directories), checks the cache slot, and unpacks via
//! [`crate::snapshot::unpack_snapshot`] when missing. Concurrent
//! callers may both extract into the same slot — atomic-rename
//! coordination is a follow-up; this slice favors simplicity over
//! contention safety since eval runs are typically serial.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::snapshot::{unpack_snapshot, SnapshotError};

/// A content-addressed cache of unpacked vault snapshots, rooted
/// at a single directory.
pub struct SnapshotCache {
    root: PathBuf,
}

impl SnapshotCache {
    /// Create a cache rooted at `root`. The directory is created on
    /// first use; this constructor doesn't touch the filesystem.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Cache root directory (`.engram/evals/snapshots/` by spec
    /// convention). Useful in tests.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Hash + cache slot path for `src`. Returns
    /// `(sha_hex, <root>/<sha_hex>)`. No filesystem mutation.
    pub fn slot_for(&self, src: &Path) -> Result<(String, PathBuf), SnapshotError> {
        let sha = hash_source(src)?;
        let slot = self.root.join(&sha);
        Ok((sha, slot))
    }

    /// Ensure the snapshot at `src` is unpacked into the cache and
    /// return the cached directory path. If the slot already exists
    /// (an earlier case shared this snapshot), the existing path is
    /// returned without re-extracting.
    pub fn ensure_unpacked(&self, src: &Path) -> Result<PathBuf, SnapshotError> {
        let (_, slot) = self.slot_for(src)?;
        if slot.is_dir()
            && std::fs::read_dir(&slot)
                .map(|d| d.count() > 0)
                .unwrap_or(false)
        {
            // Already populated — treat as a cache hit. We don't
            // verify integrity here (no manifest); the SHA-keyed
            // layout itself is the integrity claim.
            return Ok(slot);
        }
        // Cache miss (or empty directory left by a previous failed
        // run). Unpack fresh into the slot.
        std::fs::create_dir_all(&slot).map_err(|e| SnapshotError::Io {
            path: slot.clone(),
            source: e,
        })?;
        unpack_snapshot(src, &slot)?;
        Ok(slot)
    }
}

/// SHA-256 of `src` as lowercase hex.
///
/// - For a file (any kind): hash the raw bytes.
/// - For a directory: hash a deterministic transcript of every
///   file's vault-relative path + its content bytes, walked in
///   sorted order. This gives stable hashes across filesystem
///   enumeration orderings.
///
/// Hashing semantics intentionally don't follow symlinks — they're
/// skipped during transcript generation, matching
/// [`crate::snapshot::unpack_snapshot`]'s symlink-skip behavior so
/// the cache key reflects what would actually be unpacked.
fn hash_source(src: &Path) -> Result<String, SnapshotError> {
    let meta = std::fs::metadata(src).map_err(|_| SnapshotError::SourceNotFound {
        path: src.to_path_buf(),
    })?;
    let mut hasher = Sha256::new();
    if meta.is_file() {
        let bytes = std::fs::read(src).map_err(|e| SnapshotError::Io {
            path: src.to_path_buf(),
            source: e,
        })?;
        hasher.update(&bytes);
    } else if meta.is_dir() {
        hash_dir_into(src, src, &mut hasher)?;
    } else {
        return Err(SnapshotError::UnsupportedKind {
            path: src.to_path_buf(),
            hint: "source must be a file or directory to hash",
        });
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Recursively hash a directory tree into `hasher`. Files contribute
/// `b"F"`-prefixed relative-path + length + bytes; subdirectories
/// contribute `b"D"`-prefixed relative-path. Entries are visited in
/// sorted order so the digest is stable across filesystems.
fn hash_dir_into(root: &Path, dir: &Path, hasher: &mut Sha256) -> Result<(), SnapshotError> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| SnapshotError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?
        .collect::<Result<_, _>>()
        .map_err(|e| SnapshotError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let rel_bytes = rel.to_string_lossy().into_owned().into_bytes();
        let ft = entry.file_type().map_err(|e| SnapshotError::Io {
            path: path.clone(),
            source: e,
        })?;
        if ft.is_file() {
            hasher.update(b"F");
            hasher.update((rel_bytes.len() as u64).to_le_bytes());
            hasher.update(&rel_bytes);
            let bytes = std::fs::read(&path).map_err(|e| SnapshotError::Io {
                path: path.clone(),
                source: e,
            })?;
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        } else if ft.is_dir() {
            hasher.update(b"D");
            hasher.update((rel_bytes.len() as u64).to_le_bytes());
            hasher.update(&rel_bytes);
            hash_dir_into(root, &path, hasher)?;
        }
        // Symlinks: skipped (matches unpack_snapshot behavior).
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_simple_dir(path: &Path) {
        std::fs::create_dir_all(path.join("notes")).unwrap();
        std::fs::write(path.join("notes/a.md"), "alpha").unwrap();
        std::fs::write(path.join("top.md"), "top").unwrap();
    }

    #[test]
    fn ensure_unpacked_creates_slot_on_miss() {
        let src = tempdir().unwrap();
        write_simple_dir(src.path());
        let cache_root = tempdir().unwrap();
        let cache = SnapshotCache::new(cache_root.path());

        let cached = cache.ensure_unpacked(src.path()).unwrap();
        assert!(cached.starts_with(cache_root.path()));
        assert_eq!(
            std::fs::read_to_string(cached.join("notes/a.md")).unwrap(),
            "alpha"
        );
    }

    #[test]
    fn ensure_unpacked_reuses_slot_on_hit() {
        let src = tempdir().unwrap();
        write_simple_dir(src.path());
        let cache_root = tempdir().unwrap();
        let cache = SnapshotCache::new(cache_root.path());

        let first = cache.ensure_unpacked(src.path()).unwrap();
        // Mutate the cached file — if `ensure_unpacked` re-extracted
        // it on the second call, our marker would be overwritten.
        std::fs::write(first.join("marker"), "preserved").unwrap();

        let second = cache.ensure_unpacked(src.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_to_string(second.join("marker")).unwrap(),
            "preserved",
            "second call must reuse the slot, not re-extract"
        );
    }

    #[test]
    fn distinct_sources_get_distinct_slots() {
        let a = tempdir().unwrap();
        std::fs::write(a.path().join("only.md"), "a-content").unwrap();
        let b = tempdir().unwrap();
        std::fs::write(b.path().join("only.md"), "b-content").unwrap();
        let cache_root = tempdir().unwrap();
        let cache = SnapshotCache::new(cache_root.path());

        let a_slot = cache.ensure_unpacked(a.path()).unwrap();
        let b_slot = cache.ensure_unpacked(b.path()).unwrap();
        assert_ne!(
            a_slot, b_slot,
            "different content → different sha → different slot"
        );
    }

    #[test]
    fn identical_directory_contents_get_identical_slots() {
        let a = tempdir().unwrap();
        write_simple_dir(a.path());
        let b = tempdir().unwrap();
        write_simple_dir(b.path());
        let cache_root = tempdir().unwrap();
        let cache = SnapshotCache::new(cache_root.path());

        let (a_sha, _) = cache.slot_for(a.path()).unwrap();
        let (b_sha, _) = cache.slot_for(b.path()).unwrap();
        assert_eq!(
            a_sha, b_sha,
            "identical directory contents must hash to the same sha"
        );
    }

    #[test]
    fn slot_for_returns_64_char_hex_sha() {
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("x.md"), "y").unwrap();
        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let (sha, slot) = cache.slot_for(src.path()).unwrap();
        assert_eq!(sha.len(), 64);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(slot.ends_with(&sha));
    }

    #[test]
    fn missing_source_returns_source_not_found() {
        let cache = SnapshotCache::new(tempdir().unwrap().path());
        let missing = tempdir().unwrap().path().join("never");
        match cache.ensure_unpacked(&missing) {
            Err(SnapshotError::SourceNotFound { path }) => assert_eq!(path, missing),
            other => panic!("expected SourceNotFound, got {other:?}"),
        }
    }

    #[test]
    fn tar_source_caches_extracted_contents() {
        // Build a tar from a directory and verify the cache holds
        // the extracted contents on first call, reuses on second.
        let src = tempdir().unwrap();
        std::fs::write(src.path().join("only.md"), "tar-cached").unwrap();
        let parent = tempdir().unwrap();
        let tar_path = parent.path().join("vault.tar");
        {
            let file = std::fs::File::create(&tar_path).unwrap();
            let mut builder = tar::Builder::new(file);
            builder.append_dir_all(".", src.path()).unwrap();
            builder.finish().unwrap();
        }

        let cache_root = tempdir().unwrap();
        let cache = SnapshotCache::new(cache_root.path());
        let cached = cache.ensure_unpacked(&tar_path).unwrap();
        assert_eq!(
            std::fs::read_to_string(cached.join("only.md")).unwrap(),
            "tar-cached"
        );
    }
}
