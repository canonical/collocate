use collocate_core::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_LEN: usize = 4096;
const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const HEX: &[u8] = b"0123456789abcdef";

pub struct SecretStore {
    dir: PathBuf,
}

fn valid(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 128
        && !name.ends_with(".meta")
        && matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn random_from(alphabet: &[u8], len: usize) -> Result<String> {
    let limit = 256 - (256 % alphabet.len());
    let mut out = String::with_capacity(len);
    let mut urandom = File::open("/dev/urandom")?;
    let mut buf = [0u8; 64];
    while out.len() < len {
        urandom.read_exact(&mut buf)?;
        for b in buf {
            if (b as usize) < limit && out.len() < len {
                out.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    Ok(out)
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

impl SecretStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        SecretStore { dir: dir.into() }
    }

    fn path(&self, project: &str, name: &str) -> Result<PathBuf> {
        if !valid(project) || !valid(name) {
            return Err(Error::Invalid(format!("invalid secret reference {project}/{name}")));
        }
        Ok(self.dir.join(project).join(name))
    }

    fn ensure_dir(&self, project: &str) -> Result<()> {
        let dir = self.dir.join(project);
        if !dir.exists() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
            let _ = fs::set_permissions(&self.dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        }
        Ok(())
    }

    fn write_value(&self, project: &str, name: &str, value: &str) -> Result<()> {
        let path = self.path(project, name)?;
        self.ensure_dir(project)?;
        let version = self.version(project, name).unwrap_or(0) + 1;
        write_private(&path, value.as_bytes())?;
        write_private(&path.with_extension("meta"), version.to_string().as_bytes())?;
        Ok(())
    }

    pub fn ensure(&self, project: &str, name: &str, generator: &str, length: usize) -> Result<bool> {
        let path = self.path(project, name)?;
        if path.exists() {
            return Ok(false);
        }
        if length == 0 || length > MAX_LEN {
            return Err(Error::Invalid(format!("secret length must be within 1..={MAX_LEN}")));
        }
        let value = match generator {
            "password" | "token" => random_from(ALNUM, length)?,
            "hex" => random_from(HEX, length)?,
            other => return Err(Error::Invalid(format!("unsupported generator {other}"))),
        };
        self.write_value(project, name, &value)?;
        Ok(true)
    }

    pub fn set(&self, project: &str, name: &str, value: &str) -> Result<()> {
        self.write_value(project, name, value)
    }

    pub fn reveal(&self, project: &str, name: &str) -> Result<String> {
        let path = self.path(project, name)?;
        fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound(format!("secret {project}/{name}"))
            } else {
                e.into()
            }
        })
    }

    pub fn version(&self, project: &str, name: &str) -> Result<u64> {
        let meta = self.path(project, name)?.with_extension("meta");
        Ok(fs::read_to_string(meta)?.trim().parse().unwrap_or(0))
    }

    pub fn remove(&self, project: &str, name: &str) -> Result<()> {
        let path = self.path(project, name)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                let _ = fs::remove_file(path.with_extension("meta"));
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!("secret {project}/{name}"))),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self, project: Option<&str>) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let projects: Vec<String> = match project {
            Some(p) => vec![p.to_string()],
            None => match fs::read_dir(&self.dir) {
                Ok(rd) => rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect(),
                Err(_) => Vec::new(),
            },
        };
        for p in projects {
            if !valid(&p) {
                continue;
            }
            if let Ok(rd) = fs::read_dir(self.dir.join(&p)) {
                for e in rd.flatten() {
                    if let Ok(n) = e.file_name().into_string() {
                        if valid(&n) {
                            out.push(format!("{p}/{n}"));
                        }
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }
}
