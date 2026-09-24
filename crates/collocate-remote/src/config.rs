use collocate_core::{Error, Result};
use collocate_trust::{generate_client, load_identity, save_identity, Identity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const LOCAL: &str = "local";
const REMOTES: &str = "remotes.yaml";
const CLIENT_CERT: &str = "client.crt";
const CLIENT_KEY: &str = "client.key";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Remote {
    pub addresses: Vec<String>,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RemoteConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub remotes: BTreeMap<String, Remote>,
}

pub fn config_dir_from(get: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let non_empty = |k: &str| get(k).filter(|v| !v.is_empty());
    if let Some(d) = non_empty("COLLOCATE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = non_empty("SNAP_USER_COMMON") {
        return Path::new(&d).join("config");
    }
    if let Some(d) = non_empty("XDG_CONFIG_HOME") {
        return Path::new(&d).join("collocate");
    }
    Path::new(&non_empty("HOME").unwrap_or_else(|| "/root".into())).join(".config").join("collocate")
}

pub fn config_dir() -> PathBuf {
    config_dir_from(&|k| std::env::var(k).ok())
}

pub fn valid_remote_name(name: &str) -> Result<()> {
    if name.is_empty() || name == LOCAL || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(Error::Invalid(format!("remote name {name:?} must be letters, digits, '-', '_' or '.', and not {LOCAL:?}")));
    }
    Ok(())
}

impl RemoteConfig {
    pub fn load(dir: &Path) -> Result<RemoteConfig> {
        match std::fs::read_to_string(dir.join(REMOTES)) {
            Ok(t) if t.trim().is_empty() => Ok(RemoteConfig::default()),
            Ok(t) => serde_saphyr::from_str(&t).map_err(|e| Error::Parse(format!("{}: {e}", dir.join(REMOTES).display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RemoteConfig::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_saphyr::to_string(self).map_err(|e| Error::Internal(format!("render remotes: {e}")))?;
        let tmp = dir.join(format!("{REMOTES}.tmp"));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, dir.join(REMOTES))?;
        Ok(())
    }

    pub fn default_remote(&self) -> &str {
        self.default.as_deref().unwrap_or(LOCAL)
    }

    pub fn get(&self, name: &str) -> Result<&Remote> {
        self.remotes.get(name).ok_or_else(|| Error::NotFound(format!("remote {name}; add it with 'collocate remote add {name} TOKEN'")))
    }
}

pub fn client_identity(dir: &Path) -> Result<Identity> {
    if let Some(id) = load_identity(dir, CLIENT_CERT, CLIENT_KEY)? {
        return Ok(id);
    }
    let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_else(|_| "collocate".into());
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_default();
    let id = generate_client(&format!("{user}@{host}"))?;
    save_identity(dir, CLIENT_CERT, CLIENT_KEY, &id)?;
    Ok(id)
}
