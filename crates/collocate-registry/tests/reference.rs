use collocate_registry::reference::{Reference, Selector};

#[test]
fn bare_name_defaults_to_docker_hub_library() {
    let r = Reference::parse("ubuntu").unwrap();
    assert_eq!(r.registry, "registry-1.docker.io");
    assert_eq!(r.repository, "library/ubuntu");
    assert_eq!(r.selector, Selector::Tag("latest".to_string()));
}

#[test]
fn bare_name_with_tag() {
    let r = Reference::parse("ubuntu:24.04").unwrap();
    assert_eq!(r.repository, "library/ubuntu");
    assert_eq!(r.selector, Selector::Tag("24.04".to_string()));
}

#[test]
fn namespaced_docker_hub_name_is_not_prefixed() {
    let r = Reference::parse("someuser/repo:tag").unwrap();
    assert_eq!(r.registry, "registry-1.docker.io");
    assert_eq!(r.repository, "someuser/repo");
    assert_eq!(r.selector, Selector::Tag("tag".to_string()));
}

#[test]
fn explicit_registry_host_is_used() {
    let r = Reference::parse("ghcr.io/canonical/foo").unwrap();
    assert_eq!(r.registry, "ghcr.io");
    assert_eq!(r.repository, "canonical/foo");
    assert_eq!(r.selector, Selector::Tag("latest".to_string()));
}

#[test]
fn registry_host_with_port() {
    let r = Reference::parse("localhost:5000/foo:bar").unwrap();
    assert_eq!(r.registry, "localhost:5000");
    assert_eq!(r.repository, "foo");
    assert_eq!(r.selector, Selector::Tag("bar".to_string()));
}

#[test]
fn digest_pin_overrides_tag() {
    let r = Reference::parse("ghcr.io/canonical/foo:tag@sha256:deadbeef").unwrap();
    assert_eq!(r.repository, "canonical/foo");
    assert_eq!(r.selector, Selector::Digest("sha256:deadbeef".to_string()));
}

#[test]
fn registry_with_port_and_nested_path() {
    let r = Reference::parse("myregistry.example.com:5000/ns/repo:tag").unwrap();
    assert_eq!(r.registry, "myregistry.example.com:5000");
    assert_eq!(r.repository, "ns/repo");
    assert_eq!(r.selector, Selector::Tag("tag".to_string()));
}

#[test]
fn empty_reference_is_rejected() {
    assert!(Reference::parse("").is_err());
}
