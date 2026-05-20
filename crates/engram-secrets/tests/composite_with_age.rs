//! Integration: `open_with_age` returns a fully-wired composite store —
//! age primary, env fallback, audit log on every op.

use engram_secrets::{open_with_age, AuditEvent, AuditOp, SecretsStore};
use secrecy::{ExposeSecret, Secret};
use tempfile::tempdir;

fn read_audit(dir: &std::path::Path) -> Vec<AuditEvent> {
    let path = dir.join("logs").join("secrets.jsonl");
    let contents = std::fs::read_to_string(&path).expect("audit log present");
    contents
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn open_with_age_round_trip() {
    let dir = tempdir().unwrap();
    let store = open_with_age(dir.path(), Secret::new("integration-passphrase".into())).unwrap();

    store
        .set("anthropic", Secret::new("sk-abc".into()))
        .unwrap();
    assert_eq!(store.get("anthropic").unwrap().expose_secret(), "sk-abc");
    assert!(store.list().unwrap().contains(&"anthropic".to_string()));
    store.remove("anthropic").unwrap();

    // Audit log captured each operation; values never written.
    let events = read_audit(dir.path());
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].op, AuditOp::Set);
    assert!(events[0].ok);
    assert_eq!(events[1].op, AuditOp::Get);
    assert!(events[1].ok);
    assert_eq!(events[2].op, AuditOp::Remove);
    assert!(events[2].ok);
}

#[test]
fn open_with_age_falls_back_to_env_on_not_found() {
    let dir = tempdir().unwrap();
    let store = open_with_age(dir.path(), Secret::new("p".into())).unwrap();

    let key = "engram_age_composite_fallback";
    let var = engram_secrets::env_var_name(key);
    std::env::set_var(&var, "from-env");

    let v = store.get(key).expect("env fallback succeeds");
    assert_eq!(v.expose_secret(), "from-env");

    std::env::remove_var(&var);
}

#[test]
fn open_with_age_persists_across_reopens() {
    let dir = tempdir().unwrap();
    let pass = "persist-across-reopens";
    {
        let s = open_with_age(dir.path(), Secret::new(pass.into())).unwrap();
        s.set("k", Secret::new("v".into())).unwrap();
    }
    let s2 = open_with_age(dir.path(), Secret::new(pass.into())).unwrap();
    assert_eq!(s2.get("k").unwrap().expose_secret(), "v");
}
