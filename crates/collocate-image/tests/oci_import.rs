use collocate_image::config::ImageStore;
use collocate_image::import::import_archive;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};
use tar::{Builder, EntryType, Header};

fn append(b: &mut Builder<Vec<u8>>, path: &str, data: &[u8]) {
    let mut h = Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(EntryType::Regular);
    h.set_cksum();
    b.append_data(&mut h, path, data).unwrap();
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256::digest(bytes).iter().map(|x| format!("{x:02x}")).collect::<String>())
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}

fn plain_layer(files: &[(&str, &str)]) -> (Vec<u8>, String) {
    let mut b = Builder::new(Vec::new());
    for (p, c) in files {
        append(&mut b, p, c.as_bytes());
    }
    let bytes = b.into_inner().unwrap();
    let id = digest(&bytes);
    (bytes, id)
}

#[derive(Clone, Copy)]
enum Comp {
    Plain,
    Gzip,
    Zstd,
}

fn oci_archive(ref_name: &str, layers: &[(Vec<u8>, String)], cmd: &str, comp: Comp) -> Vec<u8> {
    oci_archive_with_annotation(Some(ref_name), layers, cmd, comp)
}

fn oci_archive_with_annotation(ref_name: Option<&str>, layers: &[(Vec<u8>, String)], cmd: &str, comp: Comp) -> Vec<u8> {
    let diff_ids: Vec<&String> = layers.iter().map(|(_, id)| id).collect();
    let config = serde_json::json!({
        "architecture": if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" },
        "os": "linux",
        "config": {"Cmd": [cmd], "Env": ["A=1"]},
        "rootfs": {"type": "layers", "diff_ids": diff_ids}
    })
    .to_string()
    .into_bytes();
    let config_digest = digest(&config);

    let mut layer_descs = Vec::new();
    let mut blobs: Vec<(String, Vec<u8>)> = vec![(config_digest.clone(), config.clone())];
    for (bytes, _diff_id) in layers {
        let (media_type, on_disk) = match comp {
            Comp::Plain => ("application/vnd.oci.image.layer.v1.tar", bytes.clone()),
            Comp::Gzip => ("application/vnd.oci.image.layer.v1.tar+gzip", gzip(bytes)),
            Comp::Zstd => (
                "application/vnd.oci.image.layer.v1.tar+zstd",
                ruzstd::encoding::compress_to_vec(bytes.as_slice(), ruzstd::encoding::CompressionLevel::Fastest),
            ),
        };
        let blob_digest = digest(&on_disk);
        layer_descs.push(serde_json::json!({"mediaType": media_type, "digest": blob_digest, "size": on_disk.len()}));
        blobs.push((blob_digest, on_disk));
    }

    let manifest = serde_json::json!({
        "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": config_digest, "size": config.len()},
        "layers": layer_descs,
    })
    .to_string()
    .into_bytes();
    let manifest_digest = digest(&manifest);
    blobs.push((manifest_digest.clone(), manifest.clone()));

    let annotations = match ref_name {
        Some(name) => serde_json::json!({"org.opencontainers.image.ref.name": name}),
        None => serde_json::json!({}),
    };
    let index = serde_json::json!({
        "manifests": [{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": manifest_digest,
            "size": manifest.len(),
            "platform": {"architecture": if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" }, "os": "linux"},
            "annotations": annotations,
        }]
    })
    .to_string()
    .into_bytes();

    let mut b = Builder::new(Vec::new());
    append(&mut b, "oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#);
    append(&mut b, "index.json", &index);
    for (d, bytes) in &blobs {
        let (_, hex) = d.split_once(':').unwrap();
        append(&mut b, &format!("blobs/sha256/{hex}"), bytes);
    }
    b.into_inner().unwrap()
}

#[test]
fn imports_an_oci_archive_with_plain_layers() {
    let dir = tempfile::tempdir().unwrap();
    let l1 = plain_layer(&[("bin/app", "one")]);
    let ar = oci_archive("myrock:latest", std::slice::from_ref(&l1), "/bin/app", Comp::Plain);
    let imported = import_archive(Cursor::new(ar), dir.path()).unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].name, "myrock:latest");
    assert_eq!(imported[0].config.cmd, vec!["/bin/app"]);

    let store = ImageStore::new(dir.path());
    let meta = store.get("myrock:latest").unwrap();
    assert_eq!(meta.layers, vec![l1.1]);
}

#[test]
fn imports_an_oci_archive_with_gzip_layers() {
    let dir = tempfile::tempdir().unwrap();
    let l1 = plain_layer(&[("etc/conf", "base")]);
    let ar = oci_archive("gzrock:latest", std::slice::from_ref(&l1), "/bin/app", Comp::Gzip);
    let imported = import_archive(Cursor::new(ar), dir.path()).unwrap();
    assert_eq!(imported.len(), 1);

    let ldir = dir.path().join("layers").join(l1.1.replace(':', "-"));
    assert_eq!(std::fs::read_to_string(ldir.join("etc/conf")).unwrap(), "base");
}

#[test]
fn falls_back_to_digest_when_no_ref_name_annotation() {
    let dir = tempfile::tempdir().unwrap();
    let l1 = plain_layer(&[("f", "1")]);
    let ar = oci_archive_with_annotation(None, &[l1], "/x", Comp::Plain);
    let imported = import_archive(Cursor::new(ar), dir.path()).unwrap();
    assert!(imported[0].name.starts_with("sha256:"));
}

#[test]
fn imports_an_oci_archive_with_zstd_layers() {
    let dir = tempfile::tempdir().unwrap();
    let l1 = plain_layer(&[("etc/conf", "zstd")]);
    let ar = oci_archive("zrock:latest", std::slice::from_ref(&l1), "/bin/app", Comp::Zstd);
    import_archive(Cursor::new(ar), dir.path()).unwrap();
    let ldir = dir.path().join("layers").join(l1.1.replace(':', "-"));
    assert_eq!(std::fs::read_to_string(ldir.join("etc/conf")).unwrap(), "zstd");
}

#[test]
fn a_layer_shipping_pebble_marks_the_import_as_a_rock() {
    let dir = tempfile::tempdir().unwrap();
    let base = plain_layer(&[("etc/os-release", "ubuntu")]);
    let pebble = plain_layer(&[("bin/pebble", "elf")]);
    let ar = oci_archive("realrock:1", &[base, pebble], "/usr/bin/app", Comp::Gzip);
    let imported = import_archive(Cursor::new(ar), dir.path()).unwrap();
    assert_eq!(imported[0].kind, collocate_core::spec::ImageKind::Pebble);

    let plain = plain_layer(&[("bin/app", "x")]);
    let ar = oci_archive("plain:1", &[plain], "/bin/app", Comp::Plain);
    assert_eq!(import_archive(Cursor::new(ar), dir.path()).unwrap()[0].kind, collocate_core::spec::ImageKind::Oci);
}
