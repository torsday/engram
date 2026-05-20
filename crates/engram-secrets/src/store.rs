//! [`SecretsStore`] trait + [`CompositeStore`] + [`open_default`].
//!
//! Backends implement [`SecretsStore`]. The [`CompositeStore`] glues a
//! platform-native primary backend together with the env-var fallback and
//! routes every operation through the audit log.

use std::path::Path;
use std::sync::Arc;

use crate::audit::{AuditLog, AuditOp};
use crate::env::EnvStore;
use crate::error::{Error, Result};
use crate::SecretString;

/// Operations on a backend secrets store.
///
/// Implementations must be `Send + Sync` so the store can be shared across
/// async tasks and threads. Backends do **not** log themselves; the
/// [`CompositeStore`] handles auditing.
pub trait SecretsStore: Send + Sync {
    /// Read the secret named `name`. Returns [`Error::NotFound`] if absent.
    fn get(&self, name: &str) -> Result<SecretString>;

    /// Store `value` under `name`. Overwrites if already present.
    fn set(&self, name: &str, value: SecretString) -> Result<()>;

    /// Remove the secret named `name`. Idempotent — returns `Ok(())` even if
    /// the secret did not exist.
    fn remove(&self, name: &str) -> Result<()>;

    /// List the names of all secrets the backend currently holds. May return
    /// [`Error::Unsupported`] for backends that cannot enumerate (e.g. the
    /// macOS Keychain in this crate's current implementation).
    fn list(&self) -> Result<Vec<String>>;
}

/// Validate that a secret name is non-empty and contains only
/// `[A-Za-z0-9._-]`. Returns [`Error::InvalidName`] on bad input.
///
/// The constrained character set guarantees safe round-trips through
/// keychain account fields, environment variable names, and JSON-serialized
/// audit records.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidName("name is empty".into()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(Error::InvalidName(format!(
            "name `{name}` contains characters outside [A-Za-z0-9._-]"
        )));
    }
    Ok(())
}

/// Combines a primary [`SecretsStore`] (Keychain or Secret Service) with the
/// env-var fallback.
///
/// - `get`: tries primary first; on [`Error::NotFound`] falls back to env.
/// - `set` / `remove`: target the primary only — env is read-only.
/// - `list`: union of primary and env names. If primary returns
///   [`Error::Unsupported`] (e.g. keychain), only env names are returned;
///   callers depending on full enumeration should maintain their own index.
///
/// Every operation is recorded in the audit log; values are never recorded.
pub struct CompositeStore {
    primary: Arc<dyn SecretsStore>,
    env: EnvStore,
    audit: AuditLog,
}

impl CompositeStore {
    /// Build a composite store from a primary backend and an audit log.
    pub fn new(primary: Arc<dyn SecretsStore>, audit: AuditLog) -> Self {
        Self {
            primary,
            env: EnvStore,
            audit,
        }
    }
}

impl SecretsStore for CompositeStore {
    fn get(&self, name: &str) -> Result<SecretString> {
        validate_name(name)?;
        let outcome = match self.primary.get(name) {
            Ok(v) => Ok(v),
            Err(Error::NotFound { .. }) => self.env.get(name),
            Err(e) => Err(e),
        };
        self.audit.record(AuditOp::Get, name, outcome.is_ok());
        outcome
    }

    fn set(&self, name: &str, value: SecretString) -> Result<()> {
        validate_name(name)?;
        let r = self.primary.set(name, value);
        self.audit.record(AuditOp::Set, name, r.is_ok());
        r
    }

    fn remove(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let r = self.primary.remove(name);
        self.audit.record(AuditOp::Remove, name, r.is_ok());
        r
    }

    fn list(&self) -> Result<Vec<String>> {
        let mut names = match self.primary.list() {
            Ok(v) => v,
            Err(Error::Unsupported { .. }) => Vec::new(),
            Err(e) => return Err(e),
        };
        if let Ok(env_names) = self.env.list() {
            names.extend(env_names);
        }
        names.sort();
        names.dedup();
        Ok(names)
    }
}

/// Build the platform-default [`CompositeStore`].
///
/// - macOS → [`KeychainStore`](crate::KeychainStore) primary + [`EnvStore`] fallback
/// - Linux → [`SecretServiceStore`](crate::SecretServiceStore) primary + [`EnvStore`] fallback
/// - other → [`MockStore`](crate::MockStore) primary + [`EnvStore`] fallback (best-effort
///   for unsupported platforms; in practice this means "env only" since the
///   process-memory mock is per-process and not persisted)
///
/// `engram_dir` is the engram root (typically `.engram/`). The audit log is
/// written to `<engram_dir>/logs/secrets.jsonl`; the parent directory is
/// created if absent.
pub fn open_default(engram_dir: &Path) -> Result<CompositeStore> {
    let audit = AuditLog::open(engram_dir.join("logs").join("secrets.jsonl"))?;
    let primary = build_default_primary();
    Ok(CompositeStore::new(primary, audit))
}

/// Build a [`CompositeStore`] using the [`AgeFileStore`] as the primary
/// backend, with the env-var fallback layered for reads.
///
/// Use this constructor on headless deploys (CI runners, server installs,
/// headless Linux without D-Bus) where the OS keystore is unavailable or
/// undesirable. The passphrase is supplied up front by the caller; engram's
/// CLI layer (#131) is responsible for TTY-prompting it.
///
/// The age file lives at `<engram_dir>/secrets.age`; the audit log at
/// `<engram_dir>/logs/secrets.jsonl`. Both parent directories are created
/// if absent.
pub fn open_with_age(engram_dir: &Path, passphrase: SecretString) -> Result<CompositeStore> {
    let audit = AuditLog::open(engram_dir.join("logs").join("secrets.jsonl"))?;
    let primary: Arc<dyn SecretsStore> = Arc::new(crate::age_file::AgeFileStore::open(
        engram_dir.join("secrets.age"),
        passphrase,
    ));
    Ok(CompositeStore::new(primary, audit))
}

#[cfg(target_os = "macos")]
fn build_default_primary() -> Arc<dyn SecretsStore> {
    Arc::new(crate::keychain::KeychainStore::new("engram"))
}

#[cfg(target_os = "linux")]
fn build_default_primary() -> Arc<dyn SecretsStore> {
    Arc::new(crate::secret_service::SecretServiceStore::new("engram"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn build_default_primary() -> Arc<dyn SecretsStore> {
    Arc::new(crate::mock::MockStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_alphanumeric() {
        assert!(validate_name("anthropic").is_ok());
        assert!(validate_name("openai").is_ok());
        assert!(validate_name("s3.access_key").is_ok());
        assert!(validate_name("ABC_123-xyz").is_ok());
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(matches!(validate_name(""), Err(Error::InvalidName(_))));
    }

    #[test]
    fn validate_name_rejects_special_chars() {
        for bad in [
            "with space",
            "back/slash",
            "newline\n",
            "semi;colon",
            "quote\"",
        ] {
            assert!(
                matches!(validate_name(bad), Err(Error::InvalidName(_))),
                "expected InvalidName for {bad:?}"
            );
        }
    }
}
