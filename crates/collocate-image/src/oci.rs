use collocate_core::{Error, Result};
use collocate_registry::manifest::{host_arch, select_platform, Descriptor, Index, Manifest};
use std::path::{Path, PathBuf};

pub fn is_oci_layout(staging: &Path) -> bool {
    staging.join("index.json").exists() && staging.join("oci-layout").exists()
}

pub fn blob_path(staging: &Path, digest: &str) -> Result<PathBuf> {
    let (algo, hex) = digest.split_once(':').ok_or_else(|| Error::Invalid(format!("malformed digest {digest}")))?;
    Ok(staging.join("blobs").join(algo).join(hex))
}

pub fn read_index(staging: &Path) -> Result<Index> {
    let bytes = std::fs::read(staging.join("index.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn select_manifest_entry(index: &Index) -> Result<&Descriptor> {
    let arch = host_arch();
    select_platform(index, "linux", arch).ok_or_else(|| Error::Invalid(format!("no manifest for linux/{arch} in OCI index")))
}

pub fn read_manifest(staging: &Path, digest: &str) -> Result<Manifest> {
    let bytes = std::fs::read(blob_path(staging, digest)?)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub enum LayerCompression {
    None,
    Gzip,
    Zstd,
}

pub fn layer_compression(media_type: &str) -> Result<LayerCompression> {
    if media_type.ends_with("tar+gzip") || media_type.ends_with("tar.gzip") {
        Ok(LayerCompression::Gzip)
    } else if media_type.ends_with("tar+zstd") {
        Ok(LayerCompression::Zstd)
    } else if media_type.ends_with(".tar") || media_type.ends_with("tar") {
        Ok(LayerCompression::None)
    } else {
        Err(Error::Invalid(format!("unsupported layer media type {media_type}")))
    }
}
