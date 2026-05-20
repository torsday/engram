//! macOS Keychain backend using `security-framework` (`SecGenericPassword`).
//!
//! Each secret is stored as a generic password under service `<service>`
//! (e.g. `engram`) with the secret name as the account. This namespacing
//! keeps engram secrets distinct from other applications using the user's
//! keychain.
//!
//! # Listing
//!
//! `list` returns [`Error::Unsupported`]. The keychain *can* be enumerated,
//! but doing so cleanly with `security-framework` requires walking
//! `CFDictionary` values, and the result must be filtered to engram's
//! service prefix in user space anyway. The CLI layer (`#131`) maintains its
//! own index of known names instead.

use secrecy::{ExposeSecret, Secret};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

use crate::error::{Error, Result};
use crate::store::SecretsStore;
use crate::SecretString;

/// macOS Keychain `SecretsStore`.
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// Build a store rooted at the given keychain service name.
    ///
    /// Engram uses `"engram"` in production. Tests pass a unique namespace
    /// (e.g. `"engram-test-<uuid>"`) so the system keychain is left clean.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn err(op: &'static str, e: security_framework::base::Error) -> Error {
        Error::Backend {
            backend: "keychain",
            message: format!("{op}: {e}"),
        }
    }
}

// macOS errSecItemNotFound — defined in <Security/SecBase.h>.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

impl SecretsStore for KeychainStore {
    fn get(&self, name: &str) -> Result<SecretString> {
        match get_generic_password(&self.service, name) {
            Ok(bytes) => {
                let value = String::from_utf8(bytes).map_err(|_| Error::Backend {
                    backend: "keychain",
                    message: "stored value is not valid UTF-8".to_string(),
                })?;
                Ok(Secret::new(value))
            }
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Err(Error::NotFound {
                name: name.to_string(),
            }),
            Err(e) => Err(Self::err("get", e)),
        }
    }

    fn set(&self, name: &str, value: SecretString) -> Result<()> {
        set_generic_password(&self.service, name, value.expose_secret().as_bytes())
            .map_err(|e| Self::err("set", e))
    }

    fn remove(&self, name: &str) -> Result<()> {
        match delete_generic_password(&self.service, name) {
            Ok(()) => Ok(()),
            // Idempotent: missing item is not an error.
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(Self::err("remove", e)),
        }
    }

    fn list(&self) -> Result<Vec<String>> {
        Err(Error::Unsupported {
            backend: "keychain",
            op: "list",
        })
    }
}
