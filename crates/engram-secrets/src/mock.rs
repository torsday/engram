//! In-memory `SecretsStore` for unit tests and unsupported platforms.
//!
//! Stores values as plain `String` in process memory; not for production use.
//! Useful as the primary backend in tests where the real Keychain / Secret
//! Service is unavailable or undesirable (CI, headless containers).

use std::collections::HashMap;
use std::sync::RwLock;

use secrecy::{ExposeSecret, Secret};

use crate::error::{Error, Result};
use crate::store::SecretsStore;
use crate::SecretString;

/// In-memory `SecretsStore` for tests.
///
/// Concurrent reads are lock-free w.r.t. each other (RwLock); writes are
/// serialized. Cloning the store is not currently supported — callers wrap
/// it in `Arc<MockStore>` to share across owners.
#[derive(Debug, Default)]
pub struct MockStore {
    inner: RwLock<HashMap<String, String>>,
}

impl MockStore {
    /// Construct an empty mock store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of secrets currently held. Test helper.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    /// Whether the store is empty. Test helper.
    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }
}

impl SecretsStore for MockStore {
    fn get(&self, name: &str) -> Result<SecretString> {
        let map = self.inner.read().unwrap();
        match map.get(name) {
            Some(v) => Ok(Secret::new(v.clone())),
            None => Err(Error::NotFound {
                name: name.to_string(),
            }),
        }
    }

    fn set(&self, name: &str, value: SecretString) -> Result<()> {
        let mut map = self.inner.write().unwrap();
        map.insert(name.to_string(), value.expose_secret().clone());
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<()> {
        let mut map = self.inner.write().unwrap();
        map.remove(name);
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>> {
        let map = self.inner.read().unwrap();
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let store = MockStore::new();
        assert!(store.is_empty());
        store
            .set("anthropic", Secret::new("sk-abc".to_string()))
            .unwrap();
        store
            .set("openai", Secret::new("sk-def".to_string()))
            .unwrap();
        assert_eq!(store.len(), 2);

        assert_eq!(store.get("anthropic").unwrap().expose_secret(), "sk-abc");
        assert_eq!(store.get("openai").unwrap().expose_secret(), "sk-def");

        assert_eq!(store.list().unwrap(), vec!["anthropic", "openai"]);

        store.remove("anthropic").unwrap();
        assert!(matches!(
            store.get("anthropic"),
            Err(Error::NotFound { .. })
        ));
        // Removing a missing key is idempotent.
        store.remove("anthropic").unwrap();
    }

    #[test]
    fn set_overwrites() {
        let store = MockStore::new();
        store.set("x", Secret::new("old".to_string())).unwrap();
        store.set("x", Secret::new("new".to_string())).unwrap();
        assert_eq!(store.get("x").unwrap().expose_secret(), "new");
    }
}
