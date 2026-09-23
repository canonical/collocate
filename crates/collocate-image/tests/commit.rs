use collocate_image::commit::tar_layer;
use collocate_image::extract::{extract_layer, WhiteoutMode};
use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn commit_and_extract(build: impl FnOnce(&std::path::Path), mode: WhiteoutMode) -> (tempfile::TempDir, tempfile::TempDir) {
    let src = tempfile::tempdir().unwrap();
    build(src.path());
    let mut bytes = Vec::new();
    tar_layer(src.path(), &mut bytes).unwrap();
    let dst = tempfile::tempdir().unwrap();
    extract_layer(Cursor::new(bytes), dst.path(), mode).unwrap();
    (src, dst)
}

#[test]
fn regular_files_and_nested_directories_round_trip() {
    let (_src, dst) = commit_and_extract(
        |root| {
            fs::create_dir_all(root.join("etc")).unwrap();
            fs::write(root.join("etc/passwd"), "root:x:0:0\n").unwrap();
            let mut perm = fs::metadata(root.join("etc/passwd")).unwrap().permissions();
            perm.set_mode(0o640);
            fs::set_permissions(root.join("etc/passwd"), perm).unwrap();
            fs::create_dir_all(root.join("bin")).unwrap();
            fs::write(root.join("bin/tool"), b"#!/bin/sh\n").unwrap();
            let mut perm = fs::metadata(root.join("bin/tool")).unwrap().permissions();
            perm.set_mode(0o755);
            fs::set_permissions(root.join("bin/tool"), perm).unwrap();
        },
        WhiteoutMode::Skip,
    );
    assert_eq!(fs::read_to_string(dst.path().join("etc/passwd")).unwrap(), "root:x:0:0\n");
    assert_eq!(fs::metadata(dst.path().join("etc/passwd")).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(fs::metadata(dst.path().join("bin/tool")).unwrap().permissions().mode() & 0o777, 0o755);
}

#[test]
fn symlinks_round_trip() {
    let (_src, dst) = commit_and_extract(
        |root| {
            fs::write(root.join("real"), b"x").unwrap();
            symlink("real", root.join("alias")).unwrap();
        },
        WhiteoutMode::Skip,
    );
    assert_eq!(fs::read_link(dst.path().join("alias")).unwrap().to_str(), Some("real"));
}

#[test]
fn hardlinks_round_trip() {
    let (_src, dst) = commit_and_extract(
        |root| {
            fs::write(root.join("a"), b"shared").unwrap();
            fs::hard_link(root.join("a"), root.join("b")).unwrap();
        },
        WhiteoutMode::Skip,
    );
    assert_eq!(fs::read_to_string(dst.path().join("b")).unwrap(), "shared");
    assert_eq!(fs::metadata(dst.path().join("a")).unwrap().ino(), fs::metadata(dst.path().join("b")).unwrap().ino());
}

#[test]
fn deleted_files_become_whiteout_markers() {
    if !is_root() {
        return;
    }
    let src = tempfile::tempdir().unwrap();
    fs::create_dir_all(src.path().join("dir")).unwrap();
    let path = src.path().join("dir/gone");
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mknod(c.as_ptr(), libc::S_IFCHR, 0) }, 0);

    let mut bytes = Vec::new();
    let stats = tar_layer(src.path(), &mut bytes).unwrap();
    assert_eq!(stats.whiteouts, 1);

    let skip_dst = tempfile::tempdir().unwrap();
    let skip_stats = extract_layer(Cursor::new(bytes.clone()), skip_dst.path(), WhiteoutMode::Skip).unwrap();
    assert_eq!(skip_stats.whiteouts, 1);
    assert!(!skip_dst.path().join("dir/gone").exists());

    let overlay_dst = tempfile::tempdir().unwrap();
    extract_layer(Cursor::new(bytes), overlay_dst.path(), WhiteoutMode::Overlay).unwrap();
    let meta = fs::symlink_metadata(overlay_dst.path().join("dir/gone")).unwrap();
    use std::os::unix::fs::FileTypeExt;
    assert!(meta.file_type().is_char_device());
    assert_eq!(meta.rdev(), 0);
}

#[test]
fn opaque_directories_round_trip() {
    if !is_root() {
        return;
    }
    let src = tempfile::tempdir().unwrap();
    let dir = src.path().join("dir");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("kept"), b"k").unwrap();
    let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
    let rc = unsafe { libc::setxattr(c.as_ptr(), c"trusted.overlay.opaque".as_ptr(), b"y".as_ptr() as *const libc::c_void, 1, 0) };
    if rc != 0 {
        return;
    }

    let mut bytes = Vec::new();
    let stats = tar_layer(src.path(), &mut bytes).unwrap();
    assert_eq!(stats.opaque_dirs, 1);

    let skip_dst = tempfile::tempdir().unwrap();
    let skip_stats = extract_layer(Cursor::new(bytes.clone()), skip_dst.path(), WhiteoutMode::Skip).unwrap();
    assert_eq!(skip_stats.opaque_dirs, 1);

    let overlay_dst = tempfile::tempdir().unwrap();
    extract_layer(Cursor::new(bytes), overlay_dst.path(), WhiteoutMode::Overlay).unwrap();
    let out_dir = overlay_dst.path().join("dir");
    let c = std::ffi::CString::new(out_dir.as_os_str().as_encoded_bytes()).unwrap();
    let mut buf = [0u8; 8];
    let rc = unsafe { libc::getxattr(c.as_ptr(), c"trusted.overlay.opaque".as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    assert_eq!(rc, 1);
    assert_eq!(&buf[..1], b"y");
}
