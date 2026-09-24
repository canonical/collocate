use crate::size::parse_size;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RootModeSetting {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "overlay")]
    Overlay,
    #[serde(rename = "fuse-overlay")]
    FuseOverlay,
    #[serde(rename = "bind-ro")]
    BindRo,
}

impl RootModeSetting {
    pub fn parse(s: &str) -> Result<RootModeSetting> {
        match s {
            "auto" => Ok(RootModeSetting::Auto),
            "overlay" => Ok(RootModeSetting::Overlay),
            "fuse-overlay" => Ok(RootModeSetting::FuseOverlay),
            "bind-ro" => Ok(RootModeSetting::BindRo),
            other => Err(Error::Invalid(format!("unknown root mode {other} (expected auto, overlay, fuse-overlay or bind-ro)"))),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RootModeSetting::Auto => "auto",
            RootModeSetting::Overlay => "overlay",
            RootModeSetting::FuseOverlay => "fuse-overlay",
            RootModeSetting::BindRo => "bind-ro",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pids_max: Option<u64>,
}

impl Defaults {
    pub fn is_empty(&self) -> bool {
        self.memory.is_none() && self.cpus.is_none() && self.pids_max.is_none()
    }
}

pub const DEFAULT_SUBNET: &str = "172.30.0.0/16";
pub const DEFAULT_BRIDGE: &str = "collocate0";
pub const DEFAULT_GROUP: &str = "collocate";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DaemonSettings {
    pub subnet: String,
    pub bridge: String,
    pub root_mode: RootModeSetting,
    pub group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(skip_serializing_if = "Defaults::is_empty")]
    pub defaults: Defaults,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub https_address: Option<String>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        DaemonSettings {
            subnet: DEFAULT_SUBNET.into(),
            bridge: DEFAULT_BRIDGE.into(),
            root_mode: RootModeSetting::Auto,
            group: DEFAULT_GROUP.into(),
            node_name: None,
            defaults: Defaults::default(),
            https_address: None,
        }
    }
}

pub const DEFAULT_HTTPS_PORT: u16 = 8443;

pub fn parse_listen_address(addr: &str) -> Result<(Option<std::net::IpAddr>, u16)> {
    let bad = || Error::Invalid(format!("https address {addr:?} must look like :8443, 0.0.0.0:8443, 10.0.0.5:8443 or [::]:8443"));
    let a = addr.trim();
    if let Ok(sock) = a.parse::<std::net::SocketAddr>() {
        return Ok((Some(sock.ip()), sock.port()));
    }
    if let Some(port) = a.strip_prefix(':') {
        return port.parse::<u16>().ok().filter(|p| *p > 0).map(|p| (None, p)).ok_or_else(bad);
    }
    if let Ok(ip) = a.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        return Ok((Some(ip), DEFAULT_HTTPS_PORT));
    }
    Err(bad())
}

pub fn bind_address(addr: &str) -> Result<std::net::SocketAddr> {
    let (ip, port) = parse_listen_address(addr)?;
    Ok(std::net::SocketAddr::new(ip.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)), port))
}

fn valid_bridge(name: &str) -> bool {
    !name.is_empty() && name.len() <= 15 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 63 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl DaemonSettings {
    pub fn validate(&self) -> Result<()> {
        if !valid_bridge(&self.bridge) {
            return Err(Error::Invalid(format!(
                "bridge name {:?} must be 1-15 characters of letters, digits, '-', '_' or '.'",
                self.bridge
            )));
        }
        if !valid_name(&self.group) {
            return Err(Error::Invalid(format!("group name {:?} is not valid", self.group)));
        }
        if let Some(n) = &self.node_name {
            if !valid_name(n) {
                return Err(Error::Invalid(format!("node name {n:?} is not valid")));
            }
        }
        if let Some(m) = &self.defaults.memory {
            parse_size(m)?;
        }
        if let Some(c) = self.defaults.cpus {
            if !(c > 0.0 && c.is_finite()) {
                return Err(Error::Invalid(format!("default cpus must be positive, got {c}")));
            }
        }
        if self.defaults.pids_max == Some(0) {
            return Err(Error::Invalid("default pids_max must be positive".into()));
        }
        if let Some(a) = &self.https_address {
            parse_listen_address(a)?;
        }
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string(self).map_err(|e| Error::Internal(format!("render settings: {e}")))
    }

    pub fn from_toml(text: &str) -> Result<DaemonSettings> {
        toml::from_str(text).map_err(|e| Error::Invalid(format!("settings: {e}")))
    }
}
