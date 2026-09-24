use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub run_dir: PathBuf,
    pub init_src: PathBuf,
    pub cluster_registry: PathBuf,
    pub controller_file: PathBuf,
    pub lxc: String,
    pub relay: String,
    pub snap: bool,
}

impl Layout {
    pub fn fhs() -> Layout {
        Layout {
            config: "/etc/collocate/collocated.toml".into(),
            state_dir: "/var/lib/collocate".into(),
            run_dir: "/run/collocate".into(),
            init_src: "/usr/libexec/collocate/collocate-init".into(),
            cluster_registry: "/etc/collocate/cluster.yaml".into(),
            controller_file: "/etc/collocate/collocate-compose.yaml".into(),
            lxc: "lxc".into(),
            relay: "collocate-relay".into(),
            snap: false,
        }
    }

    pub fn snap(snap: &Path, common: &Path) -> Layout {
        Layout {
            config: common.join("collocated.toml"),
            state_dir: common.join("state"),
            run_dir: common.join("run"),
            init_src: snap.join("bin").join("collocate-init"),
            cluster_registry: common.join("cluster.yaml"),
            controller_file: common.join("controller").join("collocate-compose.yaml"),
            lxc: snap.join("bin").join("lxc").to_string_lossy().into_owned(),
            relay: "collocate.relay".into(),
            snap: true,
        }
    }

    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Layout {
        match (get("SNAP").filter(|s| !s.is_empty()), get("SNAP_COMMON").filter(|s| !s.is_empty())) {
            (Some(snap), Some(common)) => Layout::snap(Path::new(&snap), Path::new(&common)),
            _ => Layout::fhs(),
        }
    }

    pub fn detect() -> Layout {
        Layout::from_env(&|k| std::env::var(k).ok())
    }

    pub fn socket(&self) -> PathBuf {
        self.run_dir.join("collocate.sock")
    }
}

pub const DAEMON_PLUGS: &[&str] =
    &["docker-privileged", "network-control", "firewall-control", "fuse-support", "system-observe", "mount-observe", "process-control"];

pub fn connect_commands(snap_name: &str, plugs: &[String]) -> Vec<String> {
    plugs.iter().map(|p| format!("sudo snap connect {snap_name}:{p}")).collect()
}
