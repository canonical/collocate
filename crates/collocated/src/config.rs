use collocate_core::layout::Layout;
use collocate_core::limits::{Limits, DEFAULT_PIDS_MAX};
use collocate_core::settings::DaemonSettings;
pub use collocate_core::settings::{Defaults, RootModeSetting};
use collocate_core::size::parse_size;
use collocate_core::{Error, Result};
use collocate_net::ipam::Subnet;
use serde::Deserialize;
use std::path::PathBuf;

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
    pub https_address: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config::with_layout(&Layout::detect())
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text).map_err(|e| Error::Invalid(format!("config: {e}")))?;
        Subnet::parse(&c.subnet)?;
        Ok(c)
    }

    pub fn with_layout(layout: &Layout) -> Config {
        Config::from_settings(layout, &DaemonSettings::default())
    }

    pub fn from_settings(layout: &Layout, s: &DaemonSettings) -> Config {
        Config {
            state_dir: layout.state_dir.clone(),
            run_dir: layout.run_dir.clone(),
            subnet: s.subnet.clone(),
            bridge: s.bridge.clone(),
            root_mode: s.root_mode,
            init_path: layout.init_src.clone(),
            cgroup_root: "/sys/fs/cgroup".into(),
            cgroup_slice: "collocate.slice".into(),
            node_name: s.node_name.clone(),
            group: s.group.clone(),
            move_self_to_supervisor: !layout.snap,
            nameserver_files: vec!["/run/systemd/resolve/resolv.conf".into(), "/etc/resolv.conf".into()],
            defaults: s.defaults.clone(),
            https_address: s.https_address.clone(),
        }
    }

    pub fn settings(&self) -> DaemonSettings {
        DaemonSettings {
            subnet: self.subnet.clone(),
            bridge: self.bridge.clone(),
            root_mode: self.root_mode,
            group: self.group.clone(),
            node_name: self.node_name.clone(),
            defaults: self.defaults.clone(),
            https_address: self.https_address.clone(),
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Option<Config>> {
        match std::fs::read_to_string(path) {
            Ok(t) => Config::from_toml(&t).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
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
