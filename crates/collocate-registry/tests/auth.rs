use collocate_registry::auth::parse_bearer_challenge;

#[test]
fn parses_realm_service_and_scope() {
    let c = parse_bearer_challenge(r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:canonical/foo:pull""#).unwrap();
    assert_eq!(c.realm, "https://ghcr.io/token");
    assert_eq!(c.service.as_deref(), Some("ghcr.io"));
    assert_eq!(c.scope.as_deref(), Some("repository:canonical/foo:pull"));
}

#[test]
fn parses_without_scope() {
    let c = parse_bearer_challenge(r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io""#).unwrap();
    assert_eq!(c.realm, "https://auth.docker.io/token");
    assert_eq!(c.scope, None);
}

#[test]
fn rejects_non_bearer_schemes() {
    assert!(parse_bearer_challenge(r#"Basic realm="registry""#).is_err());
}

#[test]
fn rejects_missing_realm() {
    assert!(parse_bearer_challenge(r#"Bearer service="x""#).is_err());
}
