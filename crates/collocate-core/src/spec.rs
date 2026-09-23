use crate::id::ContainerId;
use crate::limits::Limits;
use crate::net::Publish;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};

pub const SPEC_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Series {
    Noble,
    Resolute,
}

impl Series {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "24.04" | "noble" => Ok(Series::Noble),
            "26.04" | "resolute" => Ok(Series::Resolute),
            other => Err(Error::Invalid(format!("unsupported series {other}"))),
        }
    }

    pub fn dir_name(&self) -> &'static str {
        match self {
            Series::Noble => "noble",
            Series::Resolute => "resolute",
        }
    }

    pub fn version(&self) -> &'static str {
        match self {
            Series::Noble => "24.04",
            Series::Resolute => "26.04",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RootSource {
    Base { series: Series, build_id: String },
    Oci { digest: String, layers: Vec<String> },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageKind {
    #[default]
    Oci,
    Pebble,
}

impl ImageKind {
    pub fn is_oci(&self) -> bool {
        *self == ImageKind::Oci
    }

    pub fn label(&self) -> &'static str {
        match self {
            ImageKind::Oci => "oci",
            ImageKind::Pebble => "rock",
        }
    }
}

pub const PEBBLE_BIN: &str = "pebble";
pub const PEBBLE_DEFAULT_DIR: &str = "/var/lib/pebble/default";

fn default_user() -> String {
    "0".into()
}

fn default_workdir() -> String {
    "/".into()
}

fn default_stop_signal() -> i32 {
    15
}

fn default_stop_timeout() -> u64 {
    10
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_workdir")]
    pub workdir: String,
    #[serde(default = "default_stop_signal")]
    pub stop_signal: i32,
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Mount {
    Bind { src: String, dst: String, ro: bool },
    Tmpfs { dst: String, size: Option<u64> },
    Volume { name: String, dst: String },
    Secret { name: String, dst: String },
    Config { src: String, dst: String },
}

impl Mount {
    pub fn dst(&self) -> &str {
        match self {
            Mount::Bind { dst, .. }
            | Mount::Tmpfs { dst, .. }
            | Mount::Volume { dst, .. }
            | Mount::Secret { dst, .. }
            | Mount::Config { dst, .. } => dst,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapPolicy {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub drop: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetSpec {
    #[serde(default)]
    pub addr: Option<Ipv4Addr>,
    #[serde(default)]
    pub publish: Vec<Publish>,
    #[serde(default = "yes")]
    pub ping_group: bool,
}

impl Default for NetSpec {
    fn default() -> Self {
        NetSpec { addr: None, publish: Vec::new(), ping_group: true }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    No,
    OnFailure {
        max: u32,
    },
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum HealthKind {
    Tcp { port: u16 },
    Http { port: u16, path: String },
    Exec { argv: Vec<String> },
    Pebble {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Healthcheck {
    pub kind: HealthKind,
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub retries: u32,
    #[serde(default)]
    pub start_period_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labels {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub replica: Option<u32>,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEntry {
    pub name: String,
    pub addr: IpAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    pub schema: u32,
    pub id: ContainerId,
    pub name: String,
    pub root: RootSource,
    #[serde(default, skip_serializing_if = "ImageKind::is_oci")]
    pub image_kind: ImageKind,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    pub process: Process,
    pub hostname: String,
    #[serde(default)]
    pub extra_hosts: Vec<HostEntry>,
    #[serde(default)]
    pub dns: Vec<IpAddr>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    #[serde(default)]
    pub read_only_rootfs: bool,
    #[serde(default)]
    pub caps: CapPolicy,
    #[serde(default)]
    pub net: NetSpec,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub healthcheck: Option<Healthcheck>,
    #[serde(default)]
    pub labels: Labels,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub exit_status: Option<i32>,
}

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 63
        && matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
}

impl Spec {
    pub fn new(name: &str, root: RootSource, argv: Vec<String>) -> Self {
        Spec {
            schema: SPEC_SCHEMA,
            id: ContainerId::random().unwrap_or_else(|_| ContainerId::from_bytes([0; 6])),
            name: name.to_string(),
            root,
            image_kind: ImageKind::Oci,
            persistent: false,
            idle_timeout_secs: None,
            process: Process {
                argv,
                env: Vec::new(),
                user: default_user(),
                workdir: default_workdir(),
                stop_signal: default_stop_signal(),
                stop_timeout_secs: default_stop_timeout(),
            },
            hostname: name.to_string(),
            extra_hosts: Vec::new(),
            dns: Vec::new(),
            limits: Limits::default(),
            mounts: Vec::new(),
            read_only_rootfs: false,
            caps: CapPolicy::default(),
            net: NetSpec::default(),
            restart: RestartPolicy::default(),
            healthcheck: None,
            labels: Labels::default(),
            created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            exit_status: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(Error::InvalidSpec(m.to_string()));
        if self.name.is_empty() {
            if self.persistent {
                return bad("persistent containers need a name");
            }
        } else if !valid_name(&self.name) {
            return bad("name must match [a-z0-9][a-z0-9_.-]{0,62}");
        }
        if self.process.argv.is_empty() {
            return bad("command is empty");
        }
        self.limits.validate()?;
        let mut hosts = HashSet::new();
        for p in &self.net.publish {
            if !hosts.insert((p.host, p.proto)) {
                return bad("duplicate published host port");
            }
        }
        let mut dsts = HashSet::new();
        for m in &self.mounts {
            if !m.dst().starts_with('/') {
                return bad("mount destination must be absolute");
            }
            if !dsts.insert(m.dst().to_string()) {
                return bad("duplicate mount destination");
            }
        }
        Ok(())
    }

    pub fn pebble_dir(&self) -> String {
        let env = |k: &str| self.process.env.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
        env("PEBBLE").unwrap_or_else(|| PEBBLE_DEFAULT_DIR.to_string())
    }

    pub fn pebble_socket(&self) -> String {
        self.process
            .env
            .iter()
            .find(|(k, v)| k == "PEBBLE_SOCKET" && !v.is_empty())
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| format!("{}/.pebble.socket", self.pebble_dir()))
    }

    pub fn spec_hash(&self) -> [u8; 32] {
        let mut canonical = self.clone();
        canonical.id = ContainerId::from_bytes([0; 6]);
        canonical.created = 0;
        canonical.exit_status = None;
        canonical.labels.revision = None;
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        Sha256::digest(&bytes).into()
    }

    pub fn spec_hash_hex(&self) -> String {
        self.spec_hash().iter().map(|b| format!("{b:02x}")).collect()
    }
}
