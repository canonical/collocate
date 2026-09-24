use std::fs;

pub const APPLETS: [&str; 13] = ["sh", "cat", "ls", "echo", "grep", "id", "hostname", "touch", "wc", "sleep", "env", "true", "tee"];

pub fn docker_archive_with_busybox(tag: &str, cmd: &[&str]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    use tar::{Builder, EntryType, Header};
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let mut layer = Builder::new(Vec::new());
    let add_dir = |b: &mut Builder<Vec<u8>>, p: &str| {
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(EntryType::Directory);
        h.set_cksum();
        b.append_data(&mut h, p, std::io::empty()).unwrap();
    };
    for d in ["bin/", "etc/", "proc/", "sys/", "dev/", "run/", "tmp/", ".collocate/"] {
        add_dir(&mut layer, d);
    }
    let add_file = |b: &mut Builder<Vec<u8>>, p: &str, data: &[u8], mode: u32| {
        let mut h = Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, p, data).unwrap();
    };
    for f in ["etc/resolv.conf", "etc/hosts", "etc/hostname", ".collocate/init"] {
        add_file(&mut layer, f, b"", 0o644);
    }
    add_file(&mut layer, "etc/passwd", b"root:x:0:0:root:/root:/bin/sh\n", 0o644);
    add_file(&mut layer, "etc/group", b"root:x:0:\n", 0o644);
    add_file(&mut layer, "bin/busybox", &fs::read("/usr/bin/busybox").unwrap(), 0o755);
    for a in APPLETS {
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o777);
        h.set_entry_type(EntryType::Symlink);
        h.set_cksum();
        layer.append_link(&mut h, format!("bin/{a}"), "busybox").unwrap();
    }
    let layer_bytes = layer.into_inner().unwrap();
    let diff_id = format!("sha256:{}", hex(&Sha256::digest(&layer_bytes)));
    let config = serde_json::json!({
        "architecture": "amd64", "os": "linux",
        "config": {"Cmd": cmd, "Env": ["PATH=/bin", "FROM_IMAGE=yes"], "WorkingDir": "/tmp"},
        "rootfs": {"type": "layers", "diff_ids": [diff_id]}
    })
    .to_string();
    let cfg_name = format!("{}.json", hex(&Sha256::digest(config.as_bytes())));
    let mut outer = Builder::new(Vec::new());
    let mut put = |name: &str, data: &[u8]| {
        let mut h = Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        outer.append_data(&mut h, name, data).unwrap();
    };
    put("layer0/layer.tar", &layer_bytes);
    put(&cfg_name, config.as_bytes());
    put(
        "manifest.json",
        serde_json::json!([{"Config": cfg_name, "RepoTags": [tag], "Layers": ["layer0/layer.tar"]}]).to_string().as_bytes(),
    );
    outer.into_inner().unwrap()
}
