use collocate_core::settings::DaemonSettings;
use collocate_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_NODE_IMAGE: &str = "ubuntu:24.04";
pub const DEFAULT_INSTALL: &str = "channel:latest/stable";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Local,
    Lxd,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NodeRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LxdTarget {
    pub remote: Option<String>,
    pub project: Option<String>,
}

impl LxdTarget {
    pub fn instance(&self, name: &str) -> String {
        match &self.remote {
            Some(r) => format!("{r}:{name}"),
            None => name.to_string(),
        }
    }

    pub fn scope(&self) -> String {
        match &self.remote {
            Some(r) => format!("{r}:"),
            None => String::new(),
        }
    }

    pub fn args(&self, mut args: Vec<String>) -> Vec<String> {
        if let Some(p) = &self.project {
            args.push("--project".into());
            args.push(p.clone());
        }
        args
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClusterPreseed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing)]
    pub token: Option<String>,
    pub image: String,
    pub install: String,
    pub nodes: BTreeMap<String, NodeRecord>,
}

impl ClusterPreseed {
    pub fn target(&self) -> LxdTarget {
        LxdTarget { remote: self.remote.clone(), project: self.project.clone() }
    }
}

impl Default for ClusterPreseed {
    fn default() -> Self {
        ClusterPreseed {
            remote: None,
            project: None,
            url: None,
            token: None,
            image: DEFAULT_NODE_IMAGE.into(),
            install: DEFAULT_INSTALL.into(),
            nodes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Preseed {
    pub mode: Mode,
    pub daemon: DaemonSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<ClusterPreseed>,
}

impl Preseed {
    pub fn parse(yaml: &str) -> Result<Preseed> {
        if yaml.trim().is_empty() {
            return Ok(Preseed::default());
        }
        let p: Preseed = serde_saphyr::from_str(yaml).map_err(|e| Error::Parse(format!("preseed: {e}")))?;
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<()> {
        self.daemon.validate()?;
        if self.mode == Mode::Lxd {
            let c = self.cluster.as_ref().ok_or_else(|| Error::Invalid("mode lxd needs a cluster section".into()))?;
            if c.nodes.is_empty() {
                return Err(Error::Invalid("mode lxd needs at least one node".into()));
            }
            for name in c.nodes.keys() {
                if !valid_instance_name(name) {
                    return Err(Error::Invalid(format!("node name {name:?} is not a valid LXD instance name")));
                }
            }
            crate::provision::Install::parse(&c.install)?;
            if c.url.is_some() && c.remote.is_none() {
                return Err(Error::Invalid("cluster.url needs cluster.remote to name the remote".into()));
            }
        }
        Ok(())
    }

    pub fn to_yaml(&self) -> Result<String> {
        serde_saphyr::to_string(self).map_err(|e| Error::Internal(format!("render preseed: {e}")))
    }
}

pub fn valid_instance_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Registry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub image: String,
    pub install: String,
    pub daemon: DaemonSettings,
    pub nodes: BTreeMap<String, NodeRecord>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            remote: None,
            project: None,
            image: DEFAULT_NODE_IMAGE.into(),
            install: DEFAULT_INSTALL.into(),
            daemon: DaemonSettings::default(),
            nodes: BTreeMap::new(),
        }
    }
}

impl Registry {
    pub fn from_preseed(c: &ClusterPreseed, daemon: &DaemonSettings) -> Registry {
        let mut daemon = daemon.clone();
        daemon.node_name = None;
        Registry {
            remote: c.remote.clone(),
            project: c.project.clone(),
            image: c.image.clone(),
            install: c.install.clone(),
            daemon,
            nodes: c.nodes.clone(),
        }
    }

    pub fn target(&self) -> LxdTarget {
        LxdTarget { remote: self.remote.clone(), project: self.project.clone() }
    }

    pub fn load(path: &Path) -> Result<Option<Registry>> {
        match std::fs::read_to_string(path) {
            Ok(t) => serde_saphyr::from_str(&t).map(Some).map_err(|e| Error::Parse(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_saphyr::to_string(self).map_err(|e| Error::Internal(format!("render registry: {e}")))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("yaml.new");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn relay(&self) -> String {
        crate::provision::Install::parse(&self.install).map(|i| i.relay().to_string()).unwrap_or_else(|_| "collocate.relay".into())
    }
}
