//! Age-encrypted file backend for headless deployments.
//!
//! Stores secrets as an age-encrypted JSON object at `<engram_dir>/secrets.age`.
//! The encryption uses a single passphrase (age's scrypt recipient — symmetric,
//! single-recipient). Intended for environments where no system keystore is
//! available: CI runners, server deploys, headless Linux without D-Bus.
//!
//! # Threat model
//!
//! At-rest is strong: an attacker with file-system read access cannot recover
//! secrets without the passphrase, and scrypt's tunable cost discourages
//! brute-force.
//!
//! In-process is the weak link: the passphrase must be present in process
//! memory to decrypt secrets, and the decrypted map is cached for the
//! duration of the process (rather than re-decrypting on every `get`). A
//! compromise of the running engram process therefore exposes every secret
//! that has been read. This is documented; on user machines, prefer the
//! Keychain / Secret Service backends.
//!
//! # On-disk layout
//!
//! ```text
//! <engram_dir>/secrets.age      // age-encrypted JSON object
//! <engram_dir>/secrets.age.tmp  // transient — written then renamed
//! ```
//!
//! Writes go via the temp file → fsync → rename pattern so a process
//! killed mid-write does not leave a half-written file.
//!
//! # Concurrency
//!
//! In-process: a single `Mutex<BTreeMap>` serializes the cached state across
//! concurrent `get`/`set`/`remove` calls. The store is `Send + Sync`.
//!
//! Cross-process: engram is single-instance per vault, so no inter-process
//! locking is implemented at this layer. The atomic rename still ensures
//! that *if* a second process opened the same file mid-write, it would see
//! either the old or new contents, never a partial write.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::iter;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use secrecy::{ExposeSecret, Secret};

use crate::error::{Error, Result};
use crate::store::{validate_name, SecretsStore};
use crate::SecretString;

/// Convert our secrecy-0.8 `SecretString` into the secrecy-0.10
/// `age::secrecy::SecretString` that age 0.11 expects.
fn to_age_secret(s: &SecretString) -> age::secrecy::SecretString {
    age::secrecy::SecretString::from(s.expose_secret().clone())
}

/// `SecretsStore` backed by an age-encrypted file.
///
/// Construct with [`AgeFileStore::open`]; the underlying file is created on
/// first write and left absent until then.
pub struct AgeFileStore {
    path: PathBuf,
    passphrase: SecretString,
    state: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// Decrypted secrets. `None` means "not yet loaded"; an empty map means
    /// "loaded and confirmed empty" (or file did not exist).
    secrets: Option<BTreeMap<String, String>>,
}

impl AgeFileStore {
    /// Open an `AgeFileStore` rooted at the given file path.
    ///
    /// The file is not read at construction time; the first `get`/`set`/`list`
    /// call lazily loads it. Passing a path whose parent directory does not
    /// exist is allowed — the directory is created on first write.
    pub fn open(path: impl Into<PathBuf>, passphrase: SecretString) -> Self {
        Self {
            path: path.into(),
            passphrase,
            state: Mutex::new(Cache::default()),
        }
    }

    /// Path the store reads / writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load (and cache) the decrypted secret map. Returns an empty map if
    /// the underlying file does not exist.
    fn load(&self, cache: &mut Cache) -> Result<()> {
        if cache.secrets.is_some() {
            return Ok(());
        }
        let map = self.read_and_decrypt()?;
        cache.secrets = Some(map);
        Ok(())
    }

    fn read_and_decrypt(&self) -> Result<BTreeMap<String, String>> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(e) => return Err(Error::Io(e)),
        };
        let mut ciphertext = Vec::new();
        file.read_to_end(&mut ciphertext)?;

        let decryptor = age::Decryptor::new(&ciphertext[..]).map_err(|e| Error::Backend {
            backend: "age-file",
            message: format!("decode age header: {e}"),
        })?;

        let identity = age::scrypt::Identity::new(to_age_secret(&self.passphrase));
        let mut reader = decryptor
            .decrypt(iter::once(&identity as &dyn age::Identity))
            .map_err(|e| Error::Backend {
                backend: "age-file",
                message: format!("decrypt: {e}"),
            })?;

        let mut plaintext = Vec::new();
        reader.read_to_end(&mut plaintext)?;

        let map: BTreeMap<String, String> =
            serde_json::from_slice(&plaintext).map_err(|e| Error::Backend {
                backend: "age-file",
                message: format!("parse secrets JSON: {e}"),
            })?;
        Ok(map)
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let plaintext = serde_json::to_vec(map).map_err(|e| Error::Backend {
            backend: "age-file",
            message: format!("serialize secrets JSON: {e}"),
        })?;

        let recipient = age::scrypt::Recipient::new(to_age_secret(&self.passphrase));
        let encryptor =
            age::Encryptor::with_recipients(iter::once(&recipient as &dyn age::Recipient))
                .map_err(|e| Error::Backend {
                    backend: "age-file",
                    message: format!("build encryptor: {e}"),
                })?;

        let mut ciphertext = Vec::new();
        {
            let mut writer =
                encryptor
                    .wrap_output(&mut ciphertext)
                    .map_err(|e| Error::Backend {
                        backend: "age-file",
                        message: format!("wrap output: {e}"),
                    })?;
            writer.write_all(&plaintext)?;
            writer.finish().map_err(|e| Error::Backend {
                backend: "age-file",
                message: format!("finish encryption: {e}"),
            })?;
        }

        // Atomic write: tmp → fsync → rename.
        let tmp = self.path.with_extension("age.tmp");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            f.write_all(&ciphertext)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl SecretsStore for AgeFileStore {
    fn get(&self, name: &str) -> Result<SecretString> {
        validate_name(name)?;
        let mut state = self.state.lock().expect("age-file mutex poisoned");
        self.load(&mut state)?;
        let map = state.secrets.as_ref().expect("secrets loaded");
        match map.get(name) {
            Some(v) => Ok(Secret::new(v.clone())),
            None => Err(Error::NotFound {
                name: name.to_string(),
            }),
        }
    }

    fn set(&self, name: &str, value: SecretString) -> Result<()> {
        validate_name(name)?;
        let mut state = self.state.lock().expect("age-file mutex poisoned");
        self.load(&mut state)?;
        let map = state.secrets.as_mut().expect("secrets loaded");
        map.insert(name.to_string(), value.expose_secret().clone());
        self.save(map)
    }

    fn remove(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let mut state = self.state.lock().expect("age-file mutex poisoned");
        self.load(&mut state)?;
        let map = state.secrets.as_mut().expect("secrets loaded");
        if map.remove(name).is_none() {
            // Idempotent — no save needed.
            return Ok(());
        }
        self.save(map)
    }

    fn list(&self) -> Result<Vec<String>> {
        let mut state = self.state.lock().expect("age-file mutex poisoned");
        self.load(&mut state)?;
        let map = state.secrets.as_ref().expect("secrets loaded");
        Ok(map.keys().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store(dir: &Path) -> AgeFileStore {
        AgeFileStore::open(
            dir.join("secrets.age"),
            Secret::new("test-passphrase-123".into()),
        )
    }

    #[test]
    fn missing_file_loads_empty_map() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.list().unwrap().is_empty());
        assert!(matches!(s.get("anthropic"), Err(Error::NotFound { .. })));
    }

    #[test]
    fn set_get_remove_round_trip() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());

        s.set("anthropic", Secret::new("sk-abc".into())).unwrap();
        s.set("openai", Secret::new("sk-def".into())).unwrap();

        assert_eq!(s.get("anthropic").unwrap().expose_secret(), "sk-abc");
        assert_eq!(s.get("openai").unwrap().expose_secret(), "sk-def");
        assert_eq!(s.list().unwrap(), vec!["anthropic", "openai"]);

        s.remove("anthropic").unwrap();
        assert!(matches!(s.get("anthropic"), Err(Error::NotFound { .. })));
        // Idempotent.
        s.remove("anthropic").unwrap();
    }

    #[test]
    fn writes_are_persisted_to_disk_and_reload_correctly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.age");
        {
            let s = AgeFileStore::open(&path, Secret::new("persist-test".into()));
            s.set("k", Secret::new("v".into())).unwrap();
        }
        // Fresh store reading the same file with the same passphrase.
        let s2 = AgeFileStore::open(&path, Secret::new("persist-test".into()));
        assert_eq!(s2.get("k").unwrap().expose_secret(), "v");
    }

    #[test]
    fn wrong_passphrase_returns_backend_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.age");
        {
            let s = AgeFileStore::open(&path, Secret::new("correct".into()));
            s.set("k", Secret::new("v".into())).unwrap();
        }
        let s2 = AgeFileStore::open(&path, Secret::new("wrong".into()));
        match s2.get("k") {
            Err(Error::Backend { backend, .. }) => assert_eq!(backend, "age-file"),
            other => panic!("expected Backend error, got {other:?}"),
        }
    }

    #[test]
    fn malformed_file_returns_backend_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.age");
        fs::write(&path, b"not an age file").unwrap();
        let s = AgeFileStore::open(&path, Secret::new("any".into()));
        match s.get("k") {
            Err(Error::Backend { backend, .. }) => assert_eq!(backend, "age-file"),
            other => panic!("expected Backend error, got {other:?}"),
        }
    }

    #[test]
    fn atomic_write_leaves_no_tmp_file() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        s.set("k", Secret::new("v".into())).unwrap();
        s.set("k", Secret::new("v2".into())).unwrap();
        s.remove("k").unwrap();

        let tmp = dir.path().join("secrets.age.tmp");
        assert!(!tmp.exists(), "tmp file should not remain after writes");
    }

    #[test]
    fn invalid_name_rejected() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        assert!(matches!(
            s.set("bad name", Secret::new("v".into())),
            Err(Error::InvalidName(_))
        ));
        assert!(matches!(s.get("bad name"), Err(Error::InvalidName(_))));
    }

    #[test]
    fn list_returns_sorted_names() {
        let dir = tempdir().unwrap();
        let s = store(dir.path());
        s.set("zeta", Secret::new("z".into())).unwrap();
        s.set("alpha", Secret::new("a".into())).unwrap();
        s.set("mu", Secret::new("m".into())).unwrap();
        // BTreeMap key order = sorted.
        assert_eq!(s.list().unwrap(), vec!["alpha", "mu", "zeta"]);
    }
}
