use crate::config::{parse_config, ImageMeta, ImageStore};
use crate::extract::WhiteoutMode;
use crate::import::store_layer_from_blob;
use crate::oci::layer_compression;
use collocate_core::{Error, Result};
use collocate_registry::auth::Credentials;
use collocate_registry::{Client, Reference, Selector};
use std::fs;
use std::path::Path;

pub fn reference_name(reference: &Reference) -> String {
    let selector = match &reference.selector {
        Selector::Tag(t) => format!(":{t}"),
        Selector::Digest(d) => format!("@{d}"),
    };
    if reference.registry == "registry-1.docker.io" {
        let repo = reference.repository.strip_prefix("library/").unwrap_or(&reference.repository);
        format!("{repo}{selector}")
    } else {
        format!("{}/{}{selector}", reference.registry, reference.repository)
    }
}

fn unreachable(e: collocate_registry::Error) -> Error {
    Error::Unreachable(e.to_string())
}

pub fn pull_and_import(reference: &Reference, root: &Path, creds: &dyn Credentials) -> Result<ImageMeta> {
    let store = ImageStore::new(root);
    let name = reference_name(reference);
    let client = Client::new(&reference.registry);
    let (digest, manifest) = client.resolve_manifest(&reference.repository, &reference.selector, creds).map_err(unreachable)?;
    if let Ok(existing) = store.get(&name) {
        if existing.digest == digest && existing.layers.iter().all(|l| store.layer_dir(l).exists()) {
            return Ok(existing);
        }
    }

    let staging = root.join("tmp").join(format!("pull-{}", collocate_core::id::random_hex(8)?));
    fs::create_dir_all(&staging)?;
    let result = (|| -> Result<ImageMeta> {
        let mut config_bytes = Vec::new();
        client.get_blob(&reference.repository, &manifest.config.digest, &mut config_bytes, creds).map_err(unreachable)?;
        let mut meta = parse_config(&name, &digest, &String::from_utf8_lossy(&config_bytes))?;
        if meta.layers.len() != manifest.layers.len() {
            return Err(Error::Invalid(format!(
                "image {name}: manifest lists {} layers but the config lists {}",
                manifest.layers.len(),
                meta.layers.len()
            )));
        }
        let mode = if unsafe { libc::geteuid() } == 0 { WhiteoutMode::Overlay } else { WhiteoutMode::Skip };
        for (desc, diff_id) in manifest.layers.iter().zip(&meta.layers) {
            if store.layer_dir(diff_id).exists() {
                continue;
            }
            let compression = layer_compression(&desc.media_type)?;
            let blob_path = staging.join("layer");
            {
                let mut f = fs::File::create(&blob_path)?;
                client.get_blob(&reference.repository, &desc.digest, &mut f, creds).map_err(unreachable)?;
            }
            store_layer_from_blob(&store, &blob_path, diff_id, compression, mode)?;
            fs::remove_file(&blob_path)?;
        }
        meta.kind = crate::kind::detect(&store, &meta);
        store.put(&meta)?;
        Ok(meta)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

pub fn ensure_pulled(reference: &Reference, root: &Path, creds: &dyn Credentials) -> Result<ImageMeta> {
    let store = ImageStore::new(root);
    match store.get(&reference_name(reference)) {
        Err(Error::NotFound(_)) => pull_and_import(reference, root, creds),
        other => other,
    }
}
