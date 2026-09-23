use serde::Deserialize;
use std::collections::BTreeMap;

pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const DOCKER_MANIFEST_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

pub const ACCEPT_MANIFEST_TYPES: &str = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";

#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    pub architecture: String,
    pub os: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Descriptor {
    pub digest: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    #[serde(default)]
    pub size: u64,
    pub platform: Option<Platform>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    pub manifests: Vec<Descriptor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
}

pub fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

pub fn select_platform<'a>(index: &'a Index, os: &str, arch: &str) -> Option<&'a Descriptor> {
    index.manifests.iter().find(|m| match &m.platform {
        Some(p) => p.os == os && p.architecture == arch,
        None => index.manifests.len() == 1,
    })
}
