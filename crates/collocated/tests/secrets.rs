use collocate_core::Error;
use collocated::secrets::SecretStore;
use std::os::unix::fs::PermissionsExt;

fn store() -> (tempfile::TempDir, SecretStore) {
    let d = tempfile::tempdir().unwrap();
    let s = SecretStore::new(d.path().join("secrets"));
    (d, s)
}

#[test]
fn ensure_generates_once_and_is_idempotent() {
    let (_d, s) = store();
    let a = s.ensure("app", "db_password", "password", 24).unwrap();
    assert_eq!(s.reveal("app", "db_password").unwrap().len(), 24);
    let again = s.ensure("app", "db_password", "password", 24).unwrap();
    assert!(a);
    assert!(!again);
    let first = s.reveal("app", "db_password").unwrap();
    s.ensure("app", "db_password", "password", 30).unwrap();
    assert_eq!(s.reveal("app", "db_password").unwrap(), first);
}

#[test]
fn generated_secrets_are_random_and_use_the_requested_alphabet() {
    let (_d, s) = store();
    s.ensure("p", "a", "password", 32).unwrap();
    s.ensure("p", "b", "password", 32).unwrap();
    assert_ne!(s.reveal("p", "a").unwrap(), s.reveal("p", "b").unwrap());
    s.ensure("p", "h", "hex", 16).unwrap();
    let h = s.reveal("p", "h").unwrap();
    assert_eq!(h.len(), 16);
    assert!(h.bytes().all(|c| c.is_ascii_hexdigit()));
    s.ensure("p", "t", "token", 40).unwrap();
    assert!(s.reveal("p", "t").unwrap().bytes().all(|c| c.is_ascii_alphanumeric()));
}

#[test]
fn files_are_private() {
    let (d, s) = store();
    s.ensure("app", "pw", "password", 12).unwrap();
    let meta = std::fs::metadata(d.path().join("secrets/app/pw")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    let dir = std::fs::metadata(d.path().join("secrets/app")).unwrap();
    assert_eq!(dir.permissions().mode() & 0o777, 0o700);
}

#[test]
fn set_replaces_and_bumps_the_version() {
    let (_d, s) = store();
    s.set("app", "pw", "one").unwrap();
    let v1 = s.version("app", "pw").unwrap();
    s.set("app", "pw", "two").unwrap();
    assert_eq!(s.reveal("app", "pw").unwrap(), "two");
    assert!(s.version("app", "pw").unwrap() > v1);
}

#[test]
fn listing_and_removal() {
    let (_d, s) = store();
    s.set("a", "x", "1").unwrap();
    s.set("a", "y", "2").unwrap();
    s.set("b", "z", "3").unwrap();
    assert_eq!(s.list(Some("a")).unwrap(), vec!["a/x".to_string(), "a/y".to_string()]);
    assert_eq!(s.list(None).unwrap().len(), 3);
    s.remove("a", "x").unwrap();
    assert!(matches!(s.reveal("a", "x"), Err(Error::NotFound(_))));
    assert!(matches!(s.remove("a", "x"), Err(Error::NotFound(_))));
}

#[test]
fn names_that_could_escape_the_store_are_rejected() {
    let (_d, s) = store();
    for bad in ["../x", "a/b", "", ".", "..", "a b", ".hidden"] {
        assert!(s.set("p", bad, "v").is_err(), "{bad}");
        assert!(s.set(bad, "n", "v").is_err(), "{bad}");
    }
}

#[test]
fn unsupported_generators_and_lengths_are_rejected() {
    let (_d, s) = store();
    assert!(s.ensure("p", "n", "rot13", 10).is_err());
    assert!(s.ensure("p", "n", "password", 0).is_err());
    assert!(s.ensure("p", "n", "password", 100_000).is_err());
}

#[test]
fn missing_secrets_are_not_found() {
    let (_d, s) = store();
    assert!(matches!(s.reveal("p", "n"), Err(Error::NotFound(_))));
}
