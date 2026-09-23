use collocate_image::config::ImageStore;
use collocate_image::import::import_archive;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Cursor;
use tar::{Builder, EntryType, Header};

fn append(b: &mut Builder<Vec<u8>>, path: &str, data: &[u8]) {
    let mut h = Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(EntryType::Regular);
    h.set_cksum();
    b.append_data(&mut h, path, data).unwrap();
}

fn layer(files: &[(&str, &str)]) -> (Vec<u8>, String) {
    let mut b = Builder::new(Vec::new());
    for (p, c) in files {
        append(&mut b, p, c.as_bytes());
    }
    let bytes = b.into_inner().unwrap();
    let id = format!("sha256:{}", Sha256::digest(&bytes).iter().map(|x| format!("{x:02x}")).collect::<String>());
    (bytes, id)
}

fn docker_archive(tag: &str, layers: &[(Vec<u8>, String)], cmd: &str) -> Vec<u8> {
    let diff_ids: Vec<&String> = layers.iter().map(|(_, id)| id).collect();
    let config = serde_json::json!({
        "architecture": if std::env::consts::ARCH == "aarch64" { "arm64" } else { "amd64" },
        "os": "linux",
        "config": {"Cmd": [cmd], "Env": ["A=1"]},
        "rootfs": {"type": "layers", "diff_ids": diff_ids}
    })
    .to_string();
    let cfg_name = format!("{}.json", Sha256::digest(config.as_bytes()).iter().map(|x| format!("{x:02x}")).collect::<String>());
    let mut b = Builder::new(Vec::new());
    let mut layer_paths = Vec::new();
    for (i, (bytes, _)) in layers.iter().enumerate() {
        let p = format!("layer{i}/layer.tar");
        append(&mut b, &p, bytes);
        layer_paths.push(p);
    }
    append(&mut b, &cfg_name, config.as_bytes());
    let manifest = serde_json::json!([{"Config": cfg_name, "RepoTags": [tag], "Layers": layer_paths}]).to_string();
    append(&mut b, "manifest.json", manifest.as_bytes());
    b.into_inner().unwrap()
}

#[test]
fn importing_extracts_layers_and_records_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let l1 = layer(&[("bin/app", "one"), ("etc/conf", "base")]);
    let l2 = layer(&[("etc/conf", "override"), ("extra", "x")]);
    let ar = docker_archive("myapp:1", &[l1.clone(), l2.clone()], "/bin/app");
    let imported = import_archive(Cursor::new(ar), dir.path()).unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].name, "myapp:1");
    assert_eq!(imported[0].layers, vec![l1.1.clone(), l2.1.clone()]);

    let ldir = |id: &str| dir.path().join("layers").join(id.replace(':', "-"));
    assert_eq!(fs::read_to_string(ldir(&l1.1).join("etc/conf")).unwrap(), "base");
    assert_eq!(fs::read_to_string(ldir(&l2.1).join("etc/conf")).unwrap(), "override");

    let store = ImageStore::new(dir.path());
    let meta = store.get("myapp:1").unwrap();
    assert_eq!(meta.config.cmd, vec!["/bin/app"]);
    assert_eq!(meta.layers.len(), 2);
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn identical_layers_are_stored_once_across_images() {
    let dir = tempfile::tempdir().unwrap();
    let shared = layer(&[("shared", "s")]);
    import_archive(Cursor::new(docker_archive("a:1", std::slice::from_ref(&shared), "/a")), dir.path()).unwrap();
    let marker = dir.path().join("layers").join(shared.1.replace(':', "-")).join("marker");
    fs::write(&marker, "kept").unwrap();
    import_archive(Cursor::new(docker_archive("b:1", std::slice::from_ref(&shared), "/b")), dir.path()).unwrap();
    assert!(marker.exists(), "an existing layer must not be re-extracted");
    assert_eq!(fs::read_dir(dir.path().join("layers")).unwrap().count(), 1);
}

#[test]
fn layers_whose_digest_does_not_match_the_config_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let real = layer(&[("f", "real")]);
    let lie = (real.0.clone(), "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string());
    let r = import_archive(Cursor::new(docker_archive("bad:1", &[lie], "/x")), dir.path());
    assert!(r.is_err());
    assert!(!dir.path().join("layers").join("sha256-0000000000000000000000000000000000000000000000000000000000000000").exists());
}

#[test]
fn archives_without_a_manifest_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Builder::new(Vec::new());
    append(&mut b, "random.txt", b"x");
    assert!(import_archive(Cursor::new(b.into_inner().unwrap()), dir.path()).is_err());
}

#[test]
fn the_store_lists_removes_and_reports_unknown_images() {
    let dir = tempfile::tempdir().unwrap();
    let l = layer(&[("f", "1")]);
    import_archive(Cursor::new(docker_archive("gone:1", &[l], "/x")), dir.path()).unwrap();
    let store = ImageStore::new(dir.path());
    assert!(store.get("missing:1").is_err());
    store.remove("gone:1").unwrap();
    assert!(store.get("gone:1").is_err());
    assert!(store.remove("gone:1").is_err());
}

#[test]
fn unreferenced_layers_can_be_collected() {
    let dir = tempfile::tempdir().unwrap();
    let keep = layer(&[("k", "1")]);
    let drop_ = layer(&[("d", "2")]);
    import_archive(Cursor::new(docker_archive("keep:1", std::slice::from_ref(&keep), "/k")), dir.path()).unwrap();
    import_archive(Cursor::new(docker_archive("drop:1", std::slice::from_ref(&drop_), "/d")), dir.path()).unwrap();
    let store = ImageStore::new(dir.path());
    store.remove("drop:1").unwrap();
    let removed = store.gc().unwrap();
    assert_eq!(removed, vec![drop_.1.replace(':', "-")]);
    assert!(dir.path().join("layers").join(keep.1.replace(':', "-")).exists());
}
