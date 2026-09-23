use collocate_core::{Error, Result};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use tar::{Archive, EntryType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteoutMode {
    Overlay,
    Skip,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub files: u64,
    pub whiteouts: u64,
    pub opaque_dirs: u64,
}

const MAX_SYMLINK_DEPTH: usize = 40;

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn clean(path: &Path) -> Result<Vec<OsString>> {
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(n) => out.push(n.to_os_string()),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => return Err(Error::Invalid(format!("path traversal in layer entry {path:?}"))),
        }
    }
    Ok(out)
}

fn resolve_dir(dest: &Path, parts: &[OsString]) -> Result<Vec<OsString>> {
    let mut resolved: Vec<OsString> = Vec::new();
    let mut pending: Vec<OsString> = parts.iter().rev().cloned().collect();
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        resolved.push(part);
        let path = dest.join(resolved.iter().collect::<PathBuf>());
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                hops += 1;
                if hops > MAX_SYMLINK_DEPTH {
                    return Err(Error::Invalid("too many symbolic links in layer path".into()));
                }
                let target = fs::read_link(&path)?;
                resolved.pop();
                if target.is_absolute() {
                    resolved.clear();
                }
                let mut fresh: Vec<OsString> = Vec::new();
                for c in target.components() {
                    match c {
                        Component::Normal(n) => fresh.push(n.to_os_string()),
                        Component::ParentDir if !resolved.is_empty() => {
                            resolved.pop();
                        }
                        _ => {}
                    }
                }
                for f in fresh.into_iter().rev() {
                    pending.push(f);
                }
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                fs::remove_file(&path)?;
                fs::create_dir(&path)?;
            }
            Err(_) => fs::create_dir(&path)?,
        }
    }
    Ok(resolved)
}

fn parent_and_name(dest: &Path, rel: &[OsString]) -> Result<(PathBuf, OsString)> {
    let (name, parents) = rel.split_last().ok_or_else(|| Error::Invalid("empty layer entry name".into()))?;
    let dir = resolve_dir(dest, parents)?;
    Ok((dest.join(dir.iter().collect::<PathBuf>()), name.clone()))
}

fn remove_existing(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => Ok(()),
    }
}

fn chown(path: &Path, uid: u32, gid: u32) {
    if !is_root() {
        return;
    }
    if let Ok(p) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
        unsafe { libc::lchown(p.as_ptr(), uid, gid) };
    }
}

fn mknod_whiteout(path: &Path) -> Result<()> {
    let p = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error::Invalid("nul in path".into()))?;
    let rc = unsafe { libc::mknod(p.as_ptr(), libc::S_IFCHR, 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn set_opaque(dir: &Path) -> Result<()> {
    let p = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).map_err(|_| Error::Invalid("nul in path".into()))?;
    let rc = unsafe { libc::setxattr(p.as_ptr(), c"trusted.overlay.opaque".as_ptr(), b"y".as_ptr() as *const libc::c_void, 1, 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub fn extract_layer<R: Read>(reader: R, dest: &Path, mode: WhiteoutMode) -> Result<Stats> {
    fs::create_dir_all(dest)?;
    let mut archive = Archive::new(reader);
    let mut stats = Stats::default();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = clean(&entry.path()?)?;
        if rel.is_empty() {
            continue;
        }
        let header_mode = entry.header().mode().unwrap_or(0o644) & 0o7777;
        let (uid, gid) = (entry.header().uid().unwrap_or(0) as u32, entry.header().gid().unwrap_or(0) as u32);
        let kind = entry.header().entry_type();

        if kind == EntryType::Directory {
            let dir = resolve_dir(dest, &rel)?;
            let path = dest.join(dir.iter().collect::<PathBuf>());
            fs::set_permissions(&path, fs::Permissions::from_mode(header_mode))?;
            chown(&path, uid, gid);
            continue;
        }

        let (parent, name) = parent_and_name(dest, &rel)?;
        let name_text = name.to_string_lossy().into_owned();
        if let Some(target) = name_text.strip_prefix(".wh.") {
            if target == ".wh..opq" {
                stats.opaque_dirs += 1;
                if mode == WhiteoutMode::Overlay {
                    set_opaque(&parent)?;
                }
            } else if !target.starts_with(".wh.") {
                stats.whiteouts += 1;
                if mode == WhiteoutMode::Overlay {
                    let path = parent.join(target);
                    remove_existing(&path)?;
                    mknod_whiteout(&path)?;
                }
            }
            continue;
        }

        let path = parent.join(&name);
        stats.files += 1;
        match kind {
            EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {
                remove_existing(&path)?;
                let mut f = File::create(&path)?;
                std::io::copy(&mut entry, &mut f)?;
                drop(f);
                fs::set_permissions(&path, fs::Permissions::from_mode(header_mode))?;
                chown(&path, uid, gid);
            }
            EntryType::Symlink => {
                let target = entry.link_name()?.ok_or_else(|| Error::Invalid("symlink without target".into()))?;
                remove_existing(&path)?;
                symlink(target, &path)?;
                chown(&path, uid, gid);
            }
            EntryType::Link => {
                let target = entry.link_name()?.ok_or_else(|| Error::Invalid("hardlink without target".into()))?;
                let trel = clean(&target)?;
                let (tparent, tname) = parent_and_name(dest, &trel)?;
                remove_existing(&path)?;
                fs::hard_link(tparent.join(tname), &path)?;
            }
            EntryType::Char | EntryType::Block | EntryType::Fifo => {
                if !is_root() {
                    continue;
                }
                let p = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error::Invalid("nul in path".into()))?;
                remove_existing(&path)?;
                let ty = match kind {
                    EntryType::Char => libc::S_IFCHR,
                    EntryType::Block => libc::S_IFBLK,
                    _ => libc::S_IFIFO,
                };
                let dev = libc::makedev(entry.header().device_major()?.unwrap_or(0), entry.header().device_minor()?.unwrap_or(0));
                unsafe { libc::mknod(p.as_ptr(), ty | header_mode, dev) };
            }
            _ => {}
        }
    }
    Ok(stats)
}

pub fn extract_maybe_gzip<R: Read>(reader: R, dest: &Path, mode: WhiteoutMode) -> Result<Stats> {
    let mut buffered = BufReader::new(reader);
    let magic = buffered.fill_buf()?;
    if magic.len() >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        extract_layer(flate2::read::GzDecoder::new(buffered), dest, mode)
    } else {
        extract_layer(buffered, dest, mode)
    }
}
