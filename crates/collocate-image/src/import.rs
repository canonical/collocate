use crate::config::{parse_config, ImageMeta, ImageStore};
use crate::extract::{extract_layer, WhiteoutMode};
use crate::oci::{self, LayerCompression};
use collocate_core::{Error, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ManifestEntry {
    config: String,
    #[serde(default)]
    repo_tags: Option<Vec<String>>,
    layers: Vec<String>,
}

struct Hashing<R: Read> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sniff_compression(layer_file: &Path) -> Result<LayerCompression> {
    let mut magic = [0u8; 4];
    let n = File::open(layer_file)?.read(&mut magic)?;
    Ok(match &magic[..n] {
        [0x1f, 0x8b, ..] => LayerCompression::Gzip,
        [0x28, 0xb5, 0x2f, 0xfd] => LayerCompression::Zstd,
        _ => LayerCompression::None,
    })
}

fn store_layer_from_path(store: &ImageStore, layer_file: &Path, diff_id: &str, mode: WhiteoutMode) -> Result<()> {
    store_layer_from_blob(store, layer_file, diff_id, sniff_compression(layer_file)?, mode)
}

pub(crate) fn store_layer_from_blob(store: &ImageStore, blob_file: &Path, diff_id: &str, compression: LayerCompression, mode: WhiteoutMode) -> Result<()> {
    let final_dir = store.layer_dir(diff_id);
    if final_dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(store.layers_dir())?;
    let tmp = store.layers_dir().join(format!(".tmp-{}", diff_id.replace(':', "-")));
    let _ = fs::remove_dir_all(&tmp);

    let raw = File::open(blob_file)?;
    let boxed: Box<dyn Read> = match compression {
        LayerCompression::None => Box::new(raw),
        LayerCompression::Gzip => Box::new(flate2::read::GzDecoder::new(raw)),
        LayerCompression::Zstd => Box::new(ruzstd::decoding::StreamingDecoder::new(raw).map_err(|e| Error::Invalid(format!("zstd: {e}")))?),
    };
    let mut hashing = Hashing { inner: boxed, hasher: Sha256::new() };
    let result = extract_layer(&mut hashing, &tmp, mode);
    let mut sink = std::io::sink();
    let _ = std::io::copy(&mut hashing, &mut sink);
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let actual = format!("sha256:{}", hex(&hashing.hasher.finalize()));
    if actual != diff_id {
        let _ = fs::remove_dir_all(&tmp);
        return Err(Error::Invalid(format!("layer digest mismatch: config says {diff_id}, content is {actual}")));
    }
    fs::rename(&tmp, &final_dir)?;
    Ok(())
}

fn import_docker_archive(staging: &Path, store: &ImageStore) -> Result<Vec<ImageMeta>> {
    let mode = if unsafe { libc::geteuid() } == 0 { WhiteoutMode::Overlay } else { WhiteoutMode::Skip };
    let manifest_path = staging.join("manifest.json");
    let entries: Vec<ManifestEntry> = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let mut imported = Vec::new();
    for entry in entries {
        let config_bytes = fs::read(staging.join(&entry.config))?;
        let digest = format!("sha256:{}", hex(&Sha256::digest(&config_bytes)));
        let name = entry.repo_tags.as_ref().and_then(|t| t.first().cloned()).unwrap_or_else(|| digest.clone());
        let meta = parse_config(&name, &digest, &String::from_utf8_lossy(&config_bytes))?;
        if meta.layers.len() != entry.layers.len() {
            return Err(Error::Invalid(format!(
                "image {name}: manifest lists {} layers but the config lists {}",
                entry.layers.len(),
                meta.layers.len()
            )));
        }
        for (path, diff_id) in entry.layers.iter().zip(&meta.layers) {
            store_layer_from_path(store, &staging.join(path), diff_id, mode)?;
        }
        store.put(&meta)?;
        imported.push(meta);
    }
    Ok(imported)
}

fn import_oci_layout(staging: &Path, store: &ImageStore) -> Result<Vec<ImageMeta>> {
    let mode = if unsafe { libc::geteuid() } == 0 { WhiteoutMode::Overlay } else { WhiteoutMode::Skip };
    let index = oci::read_index(staging)?;
    let entry = oci::select_manifest_entry(&index)?;
    let manifest = oci::read_manifest(staging, &entry.digest)?;
    let config_bytes = fs::read(oci::blob_path(staging, &manifest.config.digest)?)?;
    let name = entry
        .annotations
        .get("org.opencontainers.image.ref.name")
        .cloned()
        .unwrap_or_else(|| manifest.config.digest.clone());
    let meta = parse_config(&name, &manifest.config.digest, &String::from_utf8_lossy(&config_bytes))?;
    if meta.layers.len() != manifest.layers.len() {
        return Err(Error::Invalid(format!(
            "image {name}: manifest lists {} layers but the config lists {}",
            manifest.layers.len(),
            meta.layers.len()
        )));
    }
    for (desc, diff_id) in manifest.layers.iter().zip(&meta.layers) {
        let compression = oci::layer_compression(&desc.media_type)?;
        store_layer_from_blob(store, &oci::blob_path(staging, &desc.digest)?, diff_id, compression, mode)?;
    }
    store.put(&meta)?;
    Ok(vec![meta])
}

pub fn import_archive<R: Read>(reader: R, root: &Path) -> Result<Vec<ImageMeta>> {
    let store = ImageStore::new(root);
    let staging = root.join("tmp").join(format!("import-{}", collocate_core::id::random_hex(8)?));
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<Vec<ImageMeta>> {
        tar::Archive::new(reader).unpack(&staging)?;
        if staging.join("manifest.json").exists() {
            import_docker_archive(&staging, &store)
        } else if oci::is_oci_layout(&staging) {
            import_oci_layout(&staging, &store)
        } else {
            Err(Error::Invalid("archive has neither manifest.json nor an OCI index.json; only docker-archive and oci-archive formats are supported".into()))
        }
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}
