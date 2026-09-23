use collocate_image::extract::{extract_layer, WhiteoutMode};
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use tar::{Builder, EntryType, Header};

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn file(b: &mut Builder<Vec<u8>>, path: &str, data: &[u8], mode: u32) {
    let mut h = Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(mode);
    h.set_entry_type(EntryType::Regular);
    h.set_cksum();
    b.append_data(&mut h, path, data).unwrap();
}

fn dir(b: &mut Builder<Vec<u8>>, path: &str, mode: u32) {
    let mut h = Header::new_gnu();
    h.set_size(0);
    h.set_mode(mode);
    h.set_entry_type(EntryType::Directory);
    h.set_cksum();
    b.append_data(&mut h, path, std::io::empty()).unwrap();
}

fn link(b: &mut Builder<Vec<u8>>, path: &str, target: &str, kind: EntryType) {
    let mut h = Header::new_gnu();
    h.set_size(0);
    h.set_mode(0o777);
    h.set_entry_type(kind);
    h.set_cksum();
    b.append_link(&mut h, path, target).unwrap();
}

fn extract(build: impl FnOnce(&mut Builder<Vec<u8>>)) -> (tempfile::TempDir, collocate_core::Result<collocate_image::extract::Stats>) {
    let mut b = Builder::new(Vec::new());
    build(&mut b);
    let bytes = b.into_inner().unwrap();
    let d = tempfile::tempdir().unwrap();
    let r = extract_layer(Cursor::new(bytes), d.path(), WhiteoutMode::Skip);
    (d, r)
}

#[test]
fn regular_files_directories_modes_and_links_are_preserved() {
    let (d, r) = extract(|b| {
        dir(b, "etc/", 0o755);
        file(b, "etc/passwd", b"root:x:0:0\n", 0o640);
        file(b, "bin/tool", b"#!/bin/sh\n", 0o755);
        link(b, "bin/alias", "tool", EntryType::Symlink);
        link(b, "bin/hard", "bin/tool", EntryType::Link);
    });
    r.unwrap();
    assert_eq!(fs::read_to_string(d.path().join("etc/passwd")).unwrap(), "root:x:0:0\n");
    assert_eq!(fs::metadata(d.path().join("etc/passwd")).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(fs::metadata(d.path().join("bin/tool")).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(fs::read_link(d.path().join("bin/alias")).unwrap().to_str(), Some("tool"));
    assert_eq!(fs::metadata(d.path().join("bin/hard")).unwrap().ino(), fs::metadata(d.path().join("bin/tool")).unwrap().ino());
}

#[test]
fn parent_directories_are_created_implicitly() {
    let (d, r) = extract(|b| file(b, "a/b/c/deep.txt", b"x", 0o644));
    r.unwrap();
    assert!(d.path().join("a/b/c/deep.txt").exists());
}

#[test]
fn path_traversal_is_rejected() {
    let mut b = Builder::new(Vec::new());
    let mut h = Header::new_gnu();
    h.set_size(1);
    h.set_mode(0o644);
    h.set_entry_type(EntryType::Regular);
    let name = b"../escape.txt";
    h.as_old_mut().name[..name.len()].copy_from_slice(name);
    h.set_cksum();
    b.append(&h, &b"x"[..]).unwrap();
    let bytes = b.into_inner().unwrap();
    let d = tempfile::tempdir().unwrap();
    let outside = d.path().parent().unwrap().join("escape.txt");
    let r = extract_layer(Cursor::new(bytes), d.path(), WhiteoutMode::Skip);
    assert!(r.is_err());
    assert!(!outside.exists());
}

#[test]
fn absolute_paths_are_confined_to_the_destination() {
    let mut b = Builder::new(Vec::new());
    let mut h = Header::new_gnu();
    h.set_size(1);
    h.set_mode(0o644);
    h.set_entry_type(EntryType::Regular);
    let name = b"/abs.txt";
    h.as_old_mut().name[..name.len()].copy_from_slice(name);
    h.set_cksum();
    b.append(&h, &b"x"[..]).unwrap();
    let bytes = b.into_inner().unwrap();
    let d = tempfile::tempdir().unwrap();
    extract_layer(Cursor::new(bytes), d.path(), WhiteoutMode::Skip).unwrap();
    assert!(d.path().join("abs.txt").exists());
}

#[test]
fn writing_through_a_symlink_that_points_outside_stays_confined() {
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().to_str().unwrap().to_string();
    let (d, r) = extract(|b| {
        link(b, "evil", &target, EntryType::Symlink);
        file(b, "evil/pwned.txt", b"x", 0o644);
    });
    r.unwrap();
    assert!(!outside.path().join("pwned.txt").exists(), "extraction must stay inside the layer directory");
    let confined = d.path().join(target.trim_start_matches('/')).join("pwned.txt");
    assert!(confined.exists(), "the write is resolved within the layer root");
}

#[test]
fn hardlinks_outside_the_destination_are_refused() {
    let (_d, r) = extract(|b| link(b, "steal", "../../etc/passwd", EntryType::Link));
    assert!(r.is_err());
}

#[test]
fn later_entries_replace_earlier_ones() {
    let (d, r) = extract(|b| {
        file(b, "f", b"one", 0o644);
        file(b, "f", b"two", 0o644);
    });
    r.unwrap();
    assert_eq!(fs::read_to_string(d.path().join("f")).unwrap(), "two");
}

#[test]
fn whiteouts_are_skipped_in_skip_mode_and_reported() {
    let (d, r) = extract(|b| {
        file(b, "keep", b"k", 0o644);
        file(b, "dir/.wh.gone", b"", 0o644);
        file(b, "dir/.wh..wh..opq", b"", 0o644);
    });
    let stats = r.unwrap();
    assert_eq!(stats.whiteouts, 1);
    assert_eq!(stats.opaque_dirs, 1);
    assert!(d.path().join("keep").exists());
    assert!(!d.path().join("dir/.wh.gone").exists());
    assert!(!d.path().join("dir/.wh..wh..opq").exists());
}

#[test]
fn overlay_whiteouts_become_character_devices() {
    if !is_root() {
        return;
    }
    let mut b = Builder::new(Vec::new());
    file(&mut b, "dir/.wh.gone", b"", 0o644);
    let bytes = b.into_inner().unwrap();
    let d = tempfile::tempdir().unwrap();
    extract_layer(Cursor::new(bytes), d.path(), WhiteoutMode::Overlay).unwrap();
    let meta = fs::symlink_metadata(d.path().join("dir/gone")).unwrap();
    use std::os::unix::fs::FileTypeExt;
    assert!(meta.file_type().is_char_device());
    assert_eq!(meta.rdev(), 0);
    assert!(!d.path().join("dir/.wh.gone").exists());
}

#[test]
fn ownership_is_restored_when_running_as_root() {
    if !is_root() {
        return;
    }
    let mut b = Builder::new(Vec::new());
    let mut h = Header::new_gnu();
    h.set_size(1);
    h.set_mode(0o644);
    h.set_uid(1234);
    h.set_gid(4321);
    h.set_entry_type(EntryType::Regular);
    h.set_cksum();
    b.append_data(&mut h, "owned", &b"x"[..]).unwrap();
    let bytes = b.into_inner().unwrap();
    let d = tempfile::tempdir().unwrap();
    extract_layer(Cursor::new(bytes), d.path(), WhiteoutMode::Skip).unwrap();
    let m = fs::symlink_metadata(d.path().join("owned")).unwrap();
    assert_eq!((m.uid(), m.gid()), (1234, 4321));
}

#[test]
fn gzip_compressed_layers_are_detected() {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut b = Builder::new(Vec::new());
    file(&mut b, "z.txt", b"zipped", 0o644);
    let raw = b.into_inner().unwrap();
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&raw).unwrap();
    let gz = enc.finish().unwrap();
    let d = tempfile::tempdir().unwrap();
    collocate_image::extract::extract_maybe_gzip(Cursor::new(gz), d.path(), WhiteoutMode::Skip).unwrap();
    assert_eq!(fs::read_to_string(d.path().join("z.txt")).unwrap(), "zipped");
}
