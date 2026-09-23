use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
}

impl fmt::Display for Proto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Publish {
    pub host: u16,
    pub container: u16,
    pub proto: Proto,
}

impl Publish {
    pub fn parse(s: &str) -> Result<Self> {
        let bad = || Error::InvalidPublish(s.to_string());
        let (ports, proto) = match s.split_once('/') {
            None => (s, Proto::Tcp),
            Some((p, "tcp")) => (p, Proto::Tcp),
            Some((p, "udp")) => (p, Proto::Udp),
            Some(_) => return Err(bad()),
        };
        let parts: Vec<&str> = ports.split(':').collect();
        if parts.len() != 2 {
            return Err(bad());
        }
        let port = |p: &str| p.parse::<u16>().ok().filter(|n| *n != 0).ok_or_else(bad);
        Ok(Publish { host: port(parts[0])?, container: port(parts[1])?, proto })
    }
}

impl fmt::Display for Publish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.host, self.container, self.proto)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub src: String,
    pub dst: String,
    pub ro: bool,
}

fn valid_volume_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

impl Volume {
    pub fn parse(s: &str) -> Result<Self> {
        let bad = || Error::InvalidVolume(s.to_string());
        let parts: Vec<&str> = s.split(':').collect();
        let ro = match parts.as_slice() {
            [_, _] => false,
            [_, _, "ro"] => true,
            [_, _, "rw"] => false,
            _ => return Err(bad()),
        };
        let (src, dst) = (parts[0], parts[1]);
        if src.is_empty() || !dst.starts_with('/') {
            return Err(bad());
        }
        if !src.starts_with('/') && !valid_volume_name(src) {
            return Err(bad());
        }
        Ok(Volume { src: src.to_string(), dst: dst.to_string(), ro })
    }

    pub fn is_named(&self) -> bool {
        !self.src.starts_with('/')
    }
}

pub fn veth_names(id: &crate::ContainerId) -> (String, String) {
    let short: String = id.to_string().chars().take(10).collect();
    (format!("vh{short}"), format!("vc{short}"))
}

pub fn nft_tag(id: &crate::ContainerId) -> String {
    format!("collocate:{id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
    RoundRobin,
    Random,
    SourceHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoBackends {
    Reject,
    Drop,
}
