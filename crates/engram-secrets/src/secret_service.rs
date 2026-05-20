//! Linux Secret Service backend via the `keyring` crate.
//!
//! On Linux, secrets are stored through the system D-Bus Secret Service
//! (gnome-keyring, KWallet, etc.). The `keyring` crate's `linux-native`
//! feature speaks the protocol directly, avoiding a dbus-bindings detour.
//!
//! Each secret is stored under target `engram/<name>` so engram's items are
//! easy to identify in keystore browsers.
//!
//! # Listing
//!
//! `list` returns [`Error::Unsupported`]. The `keyring` crate exposes no
//! enumeration primitive; like the macOS Keychain backend, callers needing
//! enumeration maintain their own index file in `<engram_dir>/`.

use secrecy::{ExposeSecret, Secret};

use crate::error::{Error, Result};
use crate::store::SecretsStore;
use crate::SecretString;

/// Linux Secret Service `SecretsStore`.
pub struct SecretServiceStore {
    service: String,
}

impl SecretServiceStore {
    /// Build a store rooted at the given service name. Engram uses
    /// `"engram"` in production.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, name).map_err(Self::map_err("entry"))
    }

    fn map_err(op: &'static str) -> impl FnOnce(keyring::Error) -> Error {
        move |e| Error::Backend {
            backend: "secret-service",
            message: format!("{op}: {e}"),
        }
    }
}

impl SecretsStore for SecretServiceStore {
    fn get(&self, name: &str) -> Result<SecretString> {
        let entry = self.entry(name)?;
        match entry.get_password() {
            Ok(v) => Ok(Secret::new(v)),
            Err(keyring::Error::NoEntry) => Err(Error::NotFound {
                name: name.to_string(),
            }),
            Err(e) => Err(Self::map_err("get")(e)),
        }
    }

    fn set(&self, name: &str, value: SecretString) -> Result<()> {
        let entry = self.entry(name)?;
        entry
            .set_password(value.expose_secret())
            .map_err(Self::map_err("set"))
    }

    fn remove(&self, name: &str) -> Result<()> {
        let entry = self.entry(name)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            // Idempotent: missing entry is not an error.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Self::map_err("remove")(e)),
        }
    }

    fn list(&self) -> Result<Vec<String>> {
        Err(Error::Unsupported {
            backend: "secret-service",
            op: "list",
        })
    }
}
