//! Environment variable fallback backend.
//!
//! Read-only. Maps secret names to environment variable names via
//! [`env_var_name`]:
//!
//! - `anthropic` → `ENGRAM_ANTHROPIC`
//! - `openai` → `ENGRAM_OPENAI`
//! - `s3.access_key` → `ENGRAM_S3_ACCESS_KEY`
//!
//! Documented as dev/CI use only — environment variables are visible to other
//! processes on most platforms (`ps -E` on Linux, `procfs` introspection,
//! parent-process inheritance). The keystore backends should be preferred for
//! durable storage on user machines.

use secrecy::Secret;

use crate::error::{Error, Result};
use crate::store::SecretsStore;
use crate::SecretString;

/// Convert a secret name to its environment variable name.
///
/// The transformation: uppercase the input, replace every non-alphanumeric
/// character with `_`, and prefix `ENGRAM_`.
///
/// # Examples
///
/// ```
/// use engram_secrets::env_var_name;
/// assert_eq!(env_var_name("anthropic"), "ENGRAM_ANTHROPIC");
/// assert_eq!(env_var_name("s3.access_key"), "ENGRAM_S3_ACCESS_KEY");
/// assert_eq!(env_var_name("OpenAI"), "ENGRAM_OPENAI");
/// ```
pub fn env_var_name(secret_name: &str) -> String {
    let mut s = String::with_capacity("ENGRAM_".len() + secret_name.len());
    s.push_str("ENGRAM_");
    for ch in secret_name.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_uppercase());
        } else {
            s.push('_');
        }
    }
    s
}

/// Read-only secrets backend backed by process environment variables.
///
/// All `set` and `remove` calls return [`Error::Unsupported`] — modifying the
/// process environment from a library call would not persist anyway. To
/// rotate an env-stored secret, set the variable before launching engram.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvStore;

impl SecretsStore for EnvStore {
    fn get(&self, name: &str) -> Result<SecretString> {
        let var = env_var_name(name);
        match std::env::var(&var) {
            Ok(v) => Ok(Secret::new(v)),
            Err(_) => Err(Error::NotFound {
                name: name.to_string(),
            }),
        }
    }

    fn set(&self, _name: &str, _value: SecretString) -> Result<()> {
        Err(Error::Unsupported {
            backend: "env",
            op: "set",
        })
    }

    fn remove(&self, _name: &str) -> Result<()> {
        Err(Error::Unsupported {
            backend: "env",
            op: "remove",
        })
    }

    fn list(&self) -> Result<Vec<String>> {
        let prefix = "ENGRAM_";
        let mut names: Vec<String> = std::env::vars()
            .filter_map(|(k, _)| k.strip_prefix(prefix).map(|rest| rest.to_ascii_lowercase()))
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn env_var_name_transformations() {
        assert_eq!(env_var_name("anthropic"), "ENGRAM_ANTHROPIC");
        assert_eq!(env_var_name("OpenAI"), "ENGRAM_OPENAI");
        assert_eq!(env_var_name("s3.access_key"), "ENGRAM_S3_ACCESS_KEY");
        assert_eq!(env_var_name("a-b-c"), "ENGRAM_A_B_C");
        assert_eq!(env_var_name(""), "ENGRAM_");
    }

    #[test]
    fn get_reads_from_env() {
        // Use a unique variable name to avoid clashes with parallel tests.
        let key = "engram_secrets_test_get_ok";
        let var = env_var_name(key);
        std::env::set_var(&var, "shh");
        let s = EnvStore.get(key).unwrap();
        assert_eq!(s.expose_secret(), "shh");
        std::env::remove_var(&var);
    }

    #[test]
    fn get_missing_returns_not_found() {
        let key = "engram_secrets_test_get_missing";
        // Ensure not set.
        std::env::remove_var(env_var_name(key));
        match EnvStore.get(key) {
            Err(Error::NotFound { name }) => assert_eq!(name, key),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn set_and_remove_unsupported() {
        let store = EnvStore;
        assert!(matches!(
            store.set("x", Secret::new("y".to_string())),
            Err(Error::Unsupported {
                backend: "env",
                op: "set"
            })
        ));
        assert!(matches!(
            store.remove("x"),
            Err(Error::Unsupported {
                backend: "env",
                op: "remove"
            })
        ));
    }
}
