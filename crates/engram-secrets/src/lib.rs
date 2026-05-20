//! Secrets management for engram — provider API keys and other sensitive
//! credentials.
//!
//! Per `docs/design/03-architecture.md` §Secrets management and
//! `docs/design/09-threat-model.md` §Provider API key exposure. Secrets are
//! never committed to the vault and never written to disk in plaintext.
//!
//! # Storage hierarchy
//!
//! Backends form a precedence chain, highest to lowest:
//!
//! 1. **macOS Keychain** ([`KeychainStore`], `cfg(target_os = "macos")`)
//! 2. **Linux Secret Service** ([`SecretServiceStore`], `cfg(target_os = "linux")`)
//! 3. **Age-encrypted file** ([`AgeFileStore`]) — opt-in via [`open_with_age`]
//!    for headless deploys (CI runners, server installs, headless Linux
//!    without D-Bus). At-rest encryption is strong; passphrase in process
//!    memory is the weak link.
//! 4. **Environment variables** ([`EnvStore`]) — dev/CI only, read-only.
//!    Values visible to other processes via OS introspection; documented as
//!    weaker than the keystore backends.
//!
//! [`open_default`] builds the platform-appropriate [`CompositeStore`] with
//! the env-var fallback layered for reads — this is the entry point most
//! callers want. [`open_with_age`] is the explicit headless-deploy
//! constructor; it takes a passphrase up front (TTY prompting belongs to the
//! CLI layer, #131).
//!
//! # Invariants
//!
//! - Secret values never appear in argv, logs, audit records, or error
//!   messages.
//! - [`SecretString`] holds the value in a `secrecy::Secret<String>` wrapper
//!   that zeroizes on drop and only exposes its inner value through explicit
//!   [`secrecy::ExposeSecret::expose_secret`].
//! - Every `get`/`set`/`remove` is recorded in the audit log
//!   (`<engram_dir>/logs/secrets.jsonl`) without the value. `list` is not
//!   audited (returns names only, no value access).
//!
//! # Example
//!
//! ```no_run
//! use engram_secrets::{open_default, SecretsStore, SecretString};
//! use secrecy::Secret;
//! # fn run() -> engram_secrets::Result<()> {
//! let store = open_default(std::path::Path::new(".engram"))?;
//! store.set("anthropic", Secret::new("sk-…".to_string()))?;
//! let _key = store.get("anthropic")?;
//! # Ok(()) }
//! ```

#![deny(missing_docs)]

mod age_file;
mod audit;
mod env;
mod error;
mod mock;
mod store;

#[cfg(target_os = "macos")]
mod keychain;

#[cfg(target_os = "linux")]
mod secret_service;

pub use age_file::AgeFileStore;
pub use audit::{AuditEvent, AuditLog, AuditOp};
pub use env::{env_var_name, EnvStore};
pub use error::{Error, Result};
pub use mock::MockStore;
pub use store::{open_default, open_with_age, validate_name, CompositeStore, SecretsStore};

#[cfg(target_os = "macos")]
pub use keychain::KeychainStore;

#[cfg(target_os = "linux")]
pub use secret_service::SecretServiceStore;

/// Re-exported from the `secrecy` crate. Zeroizes on drop; access via
/// [`secrecy::ExposeSecret::expose_secret`].
pub type SecretString = secrecy::SecretString;
