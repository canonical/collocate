use crate::config::{parse_config, ImageMeta, ImageStore};
use crate::extract::{extract_maybe_gzip, WhiteoutMode};
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

fn store_layer(store: &ImageStore, root: &Path, layer_file: &Path, diff_id: &str) -> Result<()> {
    let final_dir = store.layer_dir(diff_id);
    if final_dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(store.layers_dir())?;
    let tmp = store.layers_dir().join(format!(".tmp-{}", diff_id.replace(':', "-")));
    let _ = fs::remove_dir_all(&tmp);
    let mode = if unsafe { libc::geteuid() } == 0 { WhiteoutMode::Overlay } else { WhiteoutMode::Skip };

    let raw = File::open(layer_file)?;
    let mut magic = [0u8; 2];
    let gz = {
        let mut f = File::open(layer_file)?;
        f.read_exact(&mut magic).is_ok() && magic == [0x1f, 0x8b]
    };
    let boxed: Box<dyn Read> = if gz { Box::new(flate2::read::GzDecoder::new(raw)) } else { Box::new(raw) };
    let mut hashing = Hashing { inner: boxed, hasher: Sha256::new() };
    let result = extract_maybe_gzip(&mut hashing, &tmp, mode);
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
    let _ = root;
    Ok(())
}

pub fn import_archive<R: Read>(reader: R, root: &Path) -> Result<Vec<ImageMeta>> {
    let store = ImageStore::new(root);
    let staging = root.join("tmp").join(format!("import-{}", collocate_core::id::random_hex(8)?));
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<Vec<ImageMeta>> {
        tar::Archive::new(reader).unpack(&staging)?;
        let manifest_path = staging.join("manifest.json");
        if !manifest_path.exists() {
            return Err(Error::Invalid("archive has no manifest.json; only `docker save` archives are supported".into()));
        }
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
                store_layer(&store, root, &staging.join(path), diff_id)?;
            }
            store.put(&meta)?;
            imported.push(meta);
        }
        Ok(imported)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}
