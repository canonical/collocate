mod common;

use collocate_registry::auth::Anonymous;
use collocate_registry::manifest::{host_arch, OCI_INDEX, OCI_MANIFEST};
use collocate_registry::{Client, Selector};
use common::{route, FakeServer, Script};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn resolves_index_authenticates_and_fetches_blobs_with_digest_check() {
    let config_bytes = br#"{"architecture":"amd64","os":"linux","config":{},"rootfs":{"diff_ids":[]}}"#.to_vec();
    let layer_bytes = b"pretend-layer-tar-bytes".to_vec();
    let config_digest = digest(&config_bytes);
    let layer_digest = digest(&layer_bytes);

    let manifest_json = serde_json::json!({
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": config_digest, "size": config_bytes.len()},
        "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": layer_digest, "size": layer_bytes.len()}],
    })
    .to_string()
    .into_bytes();
    let manifest_digest = digest(&manifest_json);

    let index_json = serde_json::json!({
        "manifests": [{
            "mediaType": OCI_MANIFEST,
            "digest": manifest_digest,
            "size": manifest_json.len(),
            "platform": {"architecture": host_arch(), "os": "linux"},
        }]
    })
    .to_string()
    .into_bytes();

    let server = FakeServer::start(|addr| {
        let mut routes = HashMap::new();
        routes.insert(("GET".to_string(), "/v2/testrepo/manifests/latest".to_string()), route(200, OCI_INDEX, index_json));
        routes.insert(
            ("GET".to_string(), format!("/v2/testrepo/manifests/{manifest_digest}")),
            route(200, OCI_MANIFEST, manifest_json),
        );
        routes.insert(("GET".to_string(), format!("/v2/testrepo/blobs/{config_digest}")), route(200, "application/octet-stream", config_bytes.clone()));
        routes.insert(("GET".to_string(), format!("/v2/testrepo/blobs/{layer_digest}")), route(200, "application/octet-stream", layer_bytes.clone()));
        routes.insert(("GET".to_string(), "/token".to_string()), route(200, "application/json", br#"{"token":"abc123"}"#.to_vec()));

        let mut protected = HashSet::new();
        protected.insert("/v2/testrepo/manifests/latest".to_string());

        Script { routes, protected, challenge: format!(r#"Bearer realm="http://{addr}/token",service="test""#) }
    });

    let client = Client::new(&server.addr);
    let repo = "testrepo";
    let (digest_out, manifest) = client.resolve_manifest(repo, &Selector::Tag("latest".to_string()), &Anonymous).unwrap();
    assert_eq!(digest_out, manifest_digest);
    assert_eq!(manifest.layers.len(), 1);

    let mut got_config = Vec::new();
    client.get_blob(repo, &manifest.config.digest, &mut got_config, &Anonymous).unwrap();
    assert_eq!(got_config, config_bytes);

    let mut got_layer = Vec::new();
    client.get_blob(repo, &manifest.layers[0].digest, &mut got_layer, &Anonymous).unwrap();
    assert_eq!(got_layer, layer_bytes);
}

#[test]
fn blob_digest_mismatch_is_rejected() {
    let wrong_bytes = b"not-what-was-promised".to_vec();
    let claimed_digest = digest(b"something-else");

    let server = FakeServer::start(|_addr| {
        let mut routes = HashMap::new();
        routes.insert(("GET".to_string(), format!("/v2/testrepo/blobs/{claimed_digest}")), route(200, "application/octet-stream", wrong_bytes));
        Script { routes, protected: HashSet::new(), challenge: String::new() }
    });

    let client = Client::new(&server.addr);
    let mut out = Vec::new();
    let result = client.get_blob("testrepo", &claimed_digest, &mut out, &Anonymous);
    assert!(result.is_err());
}
