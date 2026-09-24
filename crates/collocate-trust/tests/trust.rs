use collocate_core::auth::Role;
use collocate_core::Error;
use collocate_trust::{
    constant_time_eq, fingerprint_pem, generate_client, generate_server, hash_secret, load_identity, save_identity, Token, TrustStore,
};

#[test]
fn certificates_have_stable_sha256_fingerprints() {
    let server = generate_server(&["10.0.0.1".into(), "host.example".into()]).unwrap();
    assert_eq!(server.fingerprint.len(), 64);
    assert_eq!(fingerprint_pem(&server.certificate).unwrap(), server.fingerprint);
    assert!(server.key.contains("PRIVATE KEY"));
    let client = generate_client("ci").unwrap();
    assert_ne!(client.fingerprint, server.fingerprint);
    assert!(fingerprint_pem("not a certificate").is_err());
    let dir = tempfile::tempdir().unwrap();
    save_identity(dir.path(), "c.crt", "c.key", &client).unwrap();
    assert_eq!(load_identity(dir.path(), "c.crt", "c.key").unwrap().unwrap(), client);
    assert!(load_identity(dir.path(), "x.crt", "x.key").unwrap().is_none());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dir.path().join("c.key")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn tokens_round_trip_and_reject_garbage() {
    let t = Token {
        client_name: "ci".into(),
        fingerprint: "a".repeat(64),
        addresses: vec!["10.0.0.1:8443".into()],
        secret: "s3cret".into(),
        expires_at: None,
        role: Role::Viewer,
        projects: vec!["web".into()],
    };
    let text = t.encode().unwrap();
    assert!(!text.contains('=') && !text.contains('+') && !text.contains('/'));
    assert_eq!(Token::decode(&text).unwrap(), t);
    assert_eq!(Token::decode(&format!("  {text}\n")).unwrap(), t);
    for bad in ["", "!!!", "e30", &Token { addresses: vec![], ..t.clone() }.encode().unwrap()] {
        assert!(Token::decode(bad).is_err(), "{bad}");
    }
    assert!(constant_time_eq(&hash_secret("x"), &hash_secret("x")));
    assert!(!constant_time_eq(&hash_secret("x"), &hash_secret("y")));
    assert!(!constant_time_eq("ab", "abc"));
}

#[test]
fn enrollment_consumes_the_token_and_trusts_the_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let store = TrustStore::open(dir.path().join("trust")).unwrap();
    let (secret, pending) = store.create_token("ci", Role::Viewer, &["web".into()], None, 100).unwrap();
    assert_eq!(pending.expires_at, None);
    assert!(!std::fs::read_to_string(dir.path().join("trust/tokens.json")).unwrap().contains(&secret));
    assert!(matches!(store.create_token("ci", Role::Admin, &[], None, 100), Err(Error::Conflict(_))));
    let client = generate_client("ci").unwrap();
    assert!(matches!(store.enroll("wrong", &client.certificate, None, 101), Err(Error::Forbidden(_))));
    let trusted = store.enroll(&secret, &client.certificate, None, 101).unwrap();
    assert_eq!((trusted.name.as_str(), trusted.role), ("ci", Role::Viewer));
    assert_eq!(trusted.projects, vec!["web".to_string()]);
    assert_eq!(trusted.fingerprint, client.fingerprint);
    assert!(store.tokens().unwrap().is_empty());
    assert!(matches!(store.enroll(&secret, &generate_client("x").unwrap().certificate, None, 102), Err(Error::Forbidden(_))));
    let caller = store.lookup(&client.fingerprint).unwrap().unwrap().caller();
    assert!(caller.allows_project(Some("web")) && !caller.allows_project(Some("db")) && !caller.allows_project(None));
    assert!(store.lookup(&"0".repeat(64)).unwrap().is_none());
}

#[test]
fn expired_tokens_are_refused_and_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let store = TrustStore::open(dir.path()).unwrap();
    let (secret, _) = store.create_token("old", Role::Operator, &[], Some(60), 1000).unwrap();
    let client = generate_client("old").unwrap();
    assert!(matches!(store.enroll(&secret, &client.certificate, None, 1060), Err(Error::Forbidden(_))));
    assert!(store.tokens().unwrap().is_empty());
    let (secret, pending) = store.create_token("fresh", Role::Operator, &[], Some(60), 1000).unwrap();
    assert_eq!(pending.expires_at, Some(1060));
    assert_eq!(store.enroll(&secret, &client.certificate, Some("renamed"), 1059).unwrap().name, "renamed");
    store.create_token("gone", Role::Operator, &[], None, 1).unwrap();
    store.revoke_token("gone").unwrap();
    assert!(store.revoke_token("gone").is_err());
    assert!(store.create_token("bad name", Role::Operator, &[], None, 1).is_err());
    assert!(store.create_token("p", Role::Operator, &["no/slash".into()], None, 1).is_err());
}

#[test]
fn certificates_can_be_added_directly_and_removed_by_name_or_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let store = TrustStore::open(dir.path()).unwrap();
    let a = generate_client("a").unwrap();
    let b = generate_client("b").unwrap();
    store.add_certificate("a", &a.certificate, Role::Admin, &[], 1).unwrap();
    store.add_certificate("b", &b.certificate, Role::Viewer, &[], 1).unwrap();
    assert!(matches!(store.add_certificate("c", &a.certificate, Role::Admin, &[], 1), Err(Error::Conflict(_))));
    assert!(matches!(store.add_certificate("a", &generate_client("z").unwrap().certificate, Role::Admin, &[], 1), Err(Error::Conflict(_))));
    assert_eq!(store.remove(&b.fingerprint[..16]).unwrap().name, "b");
    assert!(store.remove(&b.fingerprint[..6]).is_err());
    assert_eq!(store.remove("a").unwrap().fingerprint, a.fingerprint);
    assert!(store.certificates().unwrap().is_empty());
    let id = store.ensure_server_identity(&["10.1.1.1".into()]).unwrap();
    assert_eq!(store.ensure_server_identity(&[]).unwrap(), id);
}
