//! Composite-store integration: end-to-end behavior with `MockStore` as the
//! primary backend and the env-var fallback layered for reads.

use std::sync::Arc;

use engram_secrets::{
    AuditEvent, AuditLog, AuditOp, CompositeStore, EnvStore, MockStore, SecretsStore,
};
use secrecy::{ExposeSecret, Secret};
use tempfile::tempdir;

fn open_composite(dir: &std::path::Path) -> CompositeStore {
    let audit = AuditLog::open(dir.join("logs").join("secrets.jsonl")).unwrap();
    let primary: Arc<dyn SecretsStore> = Arc::new(MockStore::new());
    CompositeStore::new(primary, audit)
}

fn read_audit(dir: &std::path::Path) -> Vec<AuditEvent> {
    let path = dir.join("logs").join("secrets.jsonl");
    let contents = std::fs::read_to_string(&path).expect("audit log present");
    contents
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn set_get_remove_round_trip() {
    let dir = tempdir().unwrap();
    let store = open_composite(dir.path());

    store
        .set("anthropic", Secret::new("sk-abc".into()))
        .unwrap();
    assert_eq!(store.get("anthropic").unwrap().expose_secret(), "sk-abc");
    store.remove("anthropic").unwrap();
    assert!(store.get("anthropic").is_err());

    let events = read_audit(dir.path());
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].op, AuditOp::Set);
    assert!(events[0].ok);
    assert_eq!(events[1].op, AuditOp::Get);
    assert!(events[1].ok);
    assert_eq!(events[2].op, AuditOp::Remove);
    assert!(events[2].ok);
    assert_eq!(events[3].op, AuditOp::Get);
    assert!(!events[3].ok); // post-remove get fails
}

#[test]
fn get_falls_back_to_env_on_not_found() {
    let dir = tempdir().unwrap();
    let store = open_composite(dir.path());

    // Set via env (use a uniquely-named key so concurrent tests don't clash).
    let key = "engram_integration_fallback_test";
    let var = engram_secrets::env_var_name(key);
    std::env::set_var(&var, "from-env");

    // Primary mock store has no entry for this key — should fall back to env.
    let value = store.get(key).expect("fallback succeeds");
    assert_eq!(value.expose_secret(), "from-env");

    std::env::remove_var(&var);
}

#[test]
fn invalid_name_rejected_before_audit() {
    let dir = tempdir().unwrap();
    let store = open_composite(dir.path());

    let err = store.get("bad name").unwrap_err();
    assert!(matches!(err, engram_secrets::Error::InvalidName(_)));

    // Invalid-name validation runs before backend dispatch; we still expect
    // no audit row (audit happens on backend outcome, but we record `ok=false`
    // only for backend failures — validation errors short-circuit). Verify:
    let events_path = dir.path().join("logs").join("secrets.jsonl");
    // File may not exist yet (no successful op has happened).
    if events_path.exists() {
        let s = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            s.is_empty(),
            "expected no audit entries for validation failures, got: {s}"
        );
    }
}

#[test]
fn list_merges_primary_and_env() {
    let dir = tempdir().unwrap();
    let store = open_composite(dir.path());

    store.set("alpha", Secret::new("a".into())).unwrap();
    store.set("beta", Secret::new("b".into())).unwrap();

    // Add an env-only entry whose name should appear in the merged list.
    let var = engram_secrets::env_var_name("engram_integration_list_only_env");
    std::env::set_var(&var, "e");

    let mut names = store.list().unwrap();
    names.retain(|n| n == "alpha" || n == "beta" || n == "engram_integration_list_only_env");
    names.sort();
    assert_eq!(
        names,
        vec![
            "alpha".to_string(),
            "beta".to_string(),
            "engram_integration_list_only_env".to_string()
        ]
    );

    std::env::remove_var(&var);
}

#[test]
fn read_only_env_store_through_composite() {
    // Direct EnvStore usage (not via composite) — verifies the read-only
    // contract holds end-to-end.
    let env = EnvStore;
    assert!(matches!(
        env.set("x", Secret::new("y".into())),
        Err(engram_secrets::Error::Unsupported {
            backend: "env",
            op: "set"
        })
    ));
}
