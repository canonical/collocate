use collocate_core::limits::{Limits, DEFAULT_PIDS_MAX};
use collocate_core::size::parse_size;
use collocate_core::{Error, Result};
use collocate_net::ipam::Subnet;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum RootModeSetting {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "overlay")]
    Overlay,
    #[serde(rename = "fuse-overlay")]
    FuseOverlay,
    #[serde(rename = "bind-ro")]
    BindRo,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    pub memory: Option<String>,
    pub cpus: Option<f64>,
    pub pids_max: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub state_dir: PathBuf,
    pub run_dir: PathBuf,
    pub subnet: String,
    pub bridge: String,
    pub root_mode: RootModeSetting,
    pub init_path: PathBuf,
    pub cgroup_root: PathBuf,
    pub cgroup_slice: String,
    pub node_name: Option<String>,
    pub group: String,
    pub move_self_to_supervisor: bool,
    pub nameserver_files: Vec<PathBuf>,
    pub defaults: Defaults,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            state_dir: "/var/lib/collocate".into(),
            run_dir: "/run/collocate".into(),
            subnet: "172.30.0.0/16".into(),
            bridge: "collocate0".into(),
            root_mode: RootModeSetting::Auto,
            init_path: "/usr/libexec/collocate/collocate-init".into(),
            cgroup_root: "/sys/fs/cgroup".into(),
            cgroup_slice: "collocate.slice".into(),
            node_name: None,
            group: "collocate".into(),
            move_self_to_supervisor: true,
            nameserver_files: vec!["/run/systemd/resolve/resolv.conf".into(), "/etc/resolv.conf".into()],
            defaults: Defaults::default(),
        }
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text).map_err(|e| Error::Invalid(format!("config: {e}")))?;
        Subnet::parse(&c.subnet)?;
        Ok(c)
    }

    pub fn load(path: &std::path::Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(t) => Config::from_toml(&t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.run_dir.join("collocate.sock")
    }

    pub fn images_dir(&self) -> PathBuf {
        self.state_dir.join("images")
    }

    pub fn apply_defaults(&self, limits: &mut Limits) -> Result<()> {
        if limits.memory.is_none() {
            if let Some(m) = &self.defaults.memory {
                limits.memory = Some(parse_size(m)?);
            }
        }
        if limits.cpus_milli.is_none() {
            if let Some(c) = self.defaults.cpus {
                limits.cpus_milli = Some((c * 1000.0).round() as u32);
            }
        }
        if limits.pids_max == DEFAULT_PIDS_MAX {
            if let Some(p) = self.defaults.pids_max {
                limits.pids_max = p;
            }
        }
        Ok(())
    }
}
