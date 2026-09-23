mod common;

use collocate_image::config::ImageStore;
use collocate_core::spec::ImageKind;
use collocate_image::pull::{ensure_pulled, pull_and_import, reference_name};
use collocate_registry::auth::Anonymous;
use collocate_registry::manifest::{host_arch, OCI_MANIFEST};
use collocate_registry::Reference;
use common::{route, FakeRegistry};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tar::{Builder, EntryType, Header};

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn tar_layer(files: &[(&str, &str)]) -> Vec<u8> {
    let mut b = Builder::new(Vec::new());
    for (path, content) in files {
        let mut h = Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, path, content.as_bytes()).unwrap();
    }
    b.into_inner().unwrap()
}

fn start_registry_bounded(max_requests: usize) -> (FakeRegistry, String, String, Vec<u8>) {
    start_registry(max_requests, &[("bin/app", "one")], serde_json::json!({"Cmd": ["/bin/app"]}))
}

fn start_registry(max_requests: usize, files: &[(&str, &str)], config: serde_json::Value) -> (FakeRegistry, String, String, Vec<u8>) {
    let layer_bytes = tar_layer(files);
    let layer_digest = digest(&layer_bytes);
    let config_bytes = serde_json::json!({
        "architecture": host_arch(),
        "os": "linux",
        "config": config,
        "rootfs": {"diff_ids": [layer_digest.clone()]},
    })
    .to_string()
    .into_bytes();
    let config_digest = digest(&config_bytes);

    let manifest_json = serde_json::json!({
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": config_digest, "size": config_bytes.len()},
        "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": layer_digest, "size": layer_bytes.len()}],
    })
    .to_string()
    .into_bytes();
    let mut routes = HashMap::new();
    routes.insert("/v2/testrepo/manifests/latest".to_string(), route(OCI_MANIFEST, manifest_json));
    routes.insert(format!("/v2/testrepo/blobs/{config_digest}"), route("application/octet-stream", config_bytes.clone()));
    routes.insert(format!("/v2/testrepo/blobs/{layer_digest}"), route("application/octet-stream", layer_bytes));

    let server = FakeRegistry::start_bounded(routes, max_requests);
    (server, config_digest, layer_digest, config_bytes)
}

#[test]
fn pulls_resolves_and_imports_into_the_local_store() {
    let (server, _config_digest, layer_digest, _config_bytes) = start_registry_bounded(usize::MAX);
    let dir = tempfile::tempdir().unwrap();
    let reference = Reference { registry: server.addr.clone(), repository: "testrepo".to_string(), selector: collocate_registry::Selector::Tag("latest".to_string()) };

    let meta = pull_and_import(&reference, dir.path(), &Anonymous).unwrap();
    assert_eq!(meta.config.cmd, vec!["/bin/app"]);
    assert_eq!(meta.layers, vec![layer_digest.clone()]);

    let store = ImageStore::new(dir.path());
    let stored = store.get(&format!("{}/testrepo:latest", server.addr)).unwrap();
    assert_eq!(stored.layers, vec![layer_digest]);
}

#[test]
fn ensure_pulled_skips_network_when_already_local() {
    let (server, _config_digest, _layer_digest, _config_bytes) = start_registry_bounded(3);
    let dir = tempfile::tempdir().unwrap();
    let reference = Reference { registry: server.addr.clone(), repository: "testrepo".to_string(), selector: collocate_registry::Selector::Tag("latest".to_string()) };

    ensure_pulled(&reference, dir.path(), &Anonymous).unwrap();

    let meta = ensure_pulled(&reference, dir.path(), &Anonymous).unwrap();
    assert_eq!(meta.config.cmd, vec!["/bin/app"]);
}

fn local_ref(server: &FakeRegistry) -> Reference {
    Reference { registry: server.addr.clone(), repository: "testrepo".to_string(), selector: collocate_registry::Selector::Tag("latest".to_string()) }
}

#[test]
fn pebble_entrypoint_marks_the_image_as_a_rock() {
    let (server, ..) = start_registry(usize::MAX, &[("bin/pebble", "elf")], serde_json::json!({"Entrypoint": ["/bin/pebble", "enter", "--verbose"]}));
    let dir = tempfile::tempdir().unwrap();
    let meta = pull_and_import(&local_ref(&server), dir.path(), &Anonymous).unwrap();
    assert_eq!(meta.kind, ImageKind::Pebble);
    assert_eq!(ImageStore::new(dir.path()).get(&meta.name).unwrap().kind, ImageKind::Pebble);
}

#[test]
fn pebble_binary_in_a_layer_marks_the_image_as_a_rock() {
    let (server, ..) = start_registry(usize::MAX, &[("usr/bin/pebble", "elf")], serde_json::json!({"Cmd": ["/usr/bin/app"]}));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(pull_and_import(&local_ref(&server), dir.path(), &Anonymous).unwrap().kind, ImageKind::Pebble);
}

#[test]
fn plain_images_stay_oci() {
    let (server, ..) = start_registry_bounded(usize::MAX);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(pull_and_import(&local_ref(&server), dir.path(), &Anonymous).unwrap().kind, ImageKind::Oci);
}

#[test]
fn reference_names_are_short_for_docker_hub() {
    assert_eq!(reference_name(&Reference::parse("nginx").unwrap()), "nginx:latest");
    assert_eq!(reference_name(&Reference::parse("docker.io/ubuntu/nginx:1.24").unwrap()), "ubuntu/nginx:1.24");
    assert_eq!(reference_name(&Reference::parse("ghcr.io/canonical/charmed-postgresql:14").unwrap()), "ghcr.io/canonical/charmed-postgresql:14");
    assert_eq!(reference_name(&Reference::parse("ghcr.io/a/b@sha256:abc").unwrap()), "ghcr.io/a/b@sha256:abc");
}
