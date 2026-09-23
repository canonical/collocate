use crate::config::{ImageConfig, ImageStore};
use crate::extract::{extract_maybe_gzip, Stats, WhiteoutMode};
use collocate_core::Result;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header};

fn is_opaque(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else { return false };
    let mut buf = [0u8; 8];
    let rc = unsafe { libc::getxattr(c.as_ptr(), c"trusted.overlay.opaque".as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    rc > 0 && &buf[..rc as usize] == b"y"
}

fn set_header_meta(h: &mut Header, meta: &fs::Metadata) {
    h.set_mode(meta.permissions().mode() & 0o7777);
    h.set_uid(meta.uid() as u64);
    h.set_gid(meta.gid() as u64);
}

fn append_dir<W: Write>(b: &mut Builder<W>, rel: &Path, meta: &fs::Metadata) -> Result<()> {
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Directory);
    h.set_size(0);
    set_header_meta(&mut h, meta);
    h.set_cksum();
    let mut name = rel.to_string_lossy().into_owned();
    name.push('/');
    b.append_data(&mut h, name, std::io::empty())?;
    Ok(())
}

fn append_marker<W: Write>(b: &mut Builder<W>, path: PathBuf) -> Result<()> {
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Regular);
    h.set_size(0);
    h.set_mode(0o644);
    h.set_cksum();
    b.append_data(&mut h, path, std::io::empty())?;
    Ok(())
}

fn append_regular<W: Write>(b: &mut Builder<W>, rel: &Path, path: &Path, meta: &fs::Metadata) -> Result<()> {
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Regular);
    h.set_size(meta.len());
    set_header_meta(&mut h, meta);
    h.set_cksum();
    let mut f = fs::File::open(path)?;
    b.append_data(&mut h, rel, &mut f)?;
    Ok(())
}

fn append_symlink<W: Write>(b: &mut Builder<W>, rel: &Path, target: &Path) -> Result<()> {
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Symlink);
    h.set_size(0);
    h.set_mode(0o777);
    h.set_cksum();
    b.append_link(&mut h, rel, target)?;
    Ok(())
}

fn append_hardlink<W: Write>(b: &mut Builder<W>, rel: &Path, target: &Path, meta: &fs::Metadata) -> Result<()> {
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Link);
    h.set_size(0);
    h.set_mode(meta.permissions().mode() & 0o7777);
    h.set_cksum();
    b.append_link(&mut h, rel, target)?;
    Ok(())
}

fn walk<W: Write>(b: &mut Builder<W>, root: &Path, rel: &Path, inodes: &mut HashMap<(u64, u64), PathBuf>, stats: &mut Stats) -> Result<()> {
    let dir = root.join(rel);
    let mut entries: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    if is_opaque(&dir) {
        stats.opaque_dirs += 1;
        append_marker(b, rel.join(".wh..wh..opq"))?;
    }
    for entry in entries {
        let child_rel = rel.join(entry.file_name());
        let meta = entry.metadata()?;
        let ft = meta.file_type();
        if ft.is_dir() {
            append_dir(b, &child_rel, &meta)?;
            walk(b, root, &child_rel, inodes, stats)?;
        } else if ft.is_char_device() && meta.rdev() == 0 {
            let name = entry.file_name().to_string_lossy().into_owned();
            append_marker(b, rel.join(format!(".wh.{name}")))?;
            stats.whiteouts += 1;
        } else if ft.is_symlink() {
            let target = fs::read_link(entry.path())?;
            append_symlink(b, &child_rel, &target)?;
            stats.files += 1;
        } else if ft.is_file() && meta.nlink() > 1 {
            let key = (meta.dev(), meta.ino());
            match inodes.get(&key) {
                Some(first) => append_hardlink(b, &child_rel, first, &meta)?,
                None => {
                    inodes.insert(key, child_rel.clone());
                    append_regular(b, &child_rel, &entry.path(), &meta)?;
                }
            }
            stats.files += 1;
        } else if ft.is_file() {
            append_regular(b, &child_rel, &entry.path(), &meta)?;
            stats.files += 1;
        } else {
            return Err(collocate_core::Error::Invalid(format!("cannot commit special file {}", child_rel.display())));
        }
    }
    Ok(())
}

pub fn tar_layer<W: Write>(src: &Path, writer: W) -> Result<Stats> {
    let mut b = Builder::new(writer);
    let mut stats = Stats::default();
    let mut inodes = HashMap::new();
    walk(&mut b, src, Path::new(""), &mut inodes, &mut stats)?;
    b.into_inner()?;
    Ok(stats)
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct HashingWriter<'a, W: Write> {
    inner: W,
    hasher: &'a mut Sha256,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn commit_upper_to_store(upper: &Path, store: &ImageStore) -> Result<String> {
    fs::create_dir_all(store.layers_dir())?;
    let tmp = store.layers_dir().join(format!(".tmp-commit-{}", collocate_core::id::random_hex(8)?));
    let mut hasher = Sha256::new();
    {
        let file = fs::File::create(&tmp)?;
        let mut hashing = HashingWriter { inner: file, hasher: &mut hasher };
        tar_layer(upper, &mut hashing)?;
    }
    let digest = format!("sha256:{}", hex(&hasher.finalize()));
    let final_dir = store.layer_dir(&digest);
    if !final_dir.exists() {
        let mode = if is_root() { WhiteoutMode::Overlay } else { WhiteoutMode::Skip };
        let raw = fs::File::open(&tmp)?;
        extract_maybe_gzip(raw, &final_dir, mode)?;
    }
    let _ = fs::remove_file(&tmp);
    Ok(digest)
}

pub fn synthetic_digest(layers: &[String], config: &ImageConfig) -> Result<String> {
    let bytes = serde_json::to_vec(&(layers, config))?;
    Ok(format!("sha256:{}", hex(&Sha256::digest(bytes))))
}
