//! macOS Keychain round-trip — gated by `ENGRAM_TEST_KEYCHAIN=1`.
//!
//! Run manually on a macOS dev box:
//!
//! ```bash
//! ENGRAM_TEST_KEYCHAIN=1 cargo test -p engram-secrets --test keychain_macos
//! ```
//!
//! Uses a unique service namespace (`engram-test-<pid>-<nanos>`) so test
//! items can't collide with production engram items or with parallel test
//! runs. Cleanup is best-effort — on test panic, stale items may remain
//! under the test service namespace; flush manually with
//! `security delete-generic-password -s engram-test-*` if needed.

#![cfg(target_os = "macos")]

use engram_secrets::{Error, KeychainStore, SecretsStore};
use secrecy::{ExposeSecret, Secret};

fn keychain_enabled() -> bool {
    std::env::var("ENGRAM_TEST_KEYCHAIN").as_deref() == Ok("1")
}

fn test_service() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("engram-test-{pid}-{nanos}")
}

#[test]
fn round_trip() {
    if !keychain_enabled() {
        eprintln!("ENGRAM_TEST_KEYCHAIN=1 not set — skipping keychain round-trip");
        return;
    }
    let svc = test_service();
    let store = KeychainStore::new(svc.clone());

    // Cleanup helper run on any exit path.
    let cleanup = |s: &KeychainStore| {
        let _ = s.remove("alpha");
        let _ = s.remove("beta");
    };

    // 1. get on missing → NotFound
    let missing = store.get("alpha");
    assert!(matches!(missing, Err(Error::NotFound { .. })));

    // 2. set then get round-trip
    store
        .set("alpha", Secret::new("test-value-1".into()))
        .expect("set succeeds");
    let v = store.get("alpha").expect("get after set");
    assert_eq!(v.expose_secret(), "test-value-1");

    // 3. set overwrites (rotation pattern)
    store
        .set("alpha", Secret::new("test-value-2".into()))
        .expect("set overwrites");
    let v2 = store.get("alpha").expect("get after rotate");
    assert_eq!(v2.expose_secret(), "test-value-2");

    // 4. multiple keys under same service
    store
        .set("beta", Secret::new("beta-value".into()))
        .expect("set beta");
    assert_eq!(store.get("beta").unwrap().expose_secret(), "beta-value");
    assert_eq!(store.get("alpha").unwrap().expose_secret(), "test-value-2");

    // 5. remove is idempotent
    store.remove("alpha").expect("remove");
    store.remove("alpha").expect("remove again (idempotent)");
    assert!(matches!(store.get("alpha"), Err(Error::NotFound { .. })));

    // 6. list returns Unsupported per crate-level note
    assert!(matches!(
        store.list(),
        Err(Error::Unsupported {
            backend: "keychain",
            op: "list"
        })
    ));

    cleanup(&store);
}
