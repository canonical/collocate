use crate::id::ContainerId;
use crate::net::{Algorithm, NoBackends, Proto};
use crate::spec::{ImageKind, Spec};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Stopped,
    Starting,
    Exited,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    #[default]
    Auto,
    Captured,
    Pebble,
}

impl LogSource {
    pub fn from_flags(raw: bool, services: &[String]) -> LogSource {
        if raw {
            LogSource::Captured
        } else if services.is_empty() {
            LogSource::Auto
        } else {
            LogSource::Pebble
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: ContainerId,
    pub name: String,
    pub state: State,
    pub pid: Option<u32>,
    pub address: Option<Ipv4Addr>,
    pub project: Option<String>,
    pub service: Option<String>,
    pub health: Option<HealthState>,
    pub revision: Option<String>,
    pub series: Option<String>,
    pub published: Vec<String>,
    #[serde(default)]
    pub image_kind: Option<ImageKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerStats {
    pub id: ContainerId,
    pub name: String,
    pub project: Option<String>,
    pub service: Option<String>,
    pub cpu_usage_usec: u64,
    pub memory_current: u64,
    pub memory_max: Option<u64>,
    pub pids: u64,
    pub cpu_limit_milli: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LbSpec {
    pub project: String,
    pub name: String,
    pub proto: Proto,
    pub listen: u16,
    pub publish: Vec<u16>,
    pub backend_service: String,
    pub backend_port: u16,
    pub algorithm: Algorithm,
    pub on_no_backends: NoBackends,
    pub drain_secs: u64,
    pub vip: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LbStatus {
    pub spec: LbSpec,
    pub vip: Ipv4Addr,
    pub backends: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    Run(Box<Spec>),
    Start {
        target: String,
    },
    Stop {
        target: String,
        timeout_secs: Option<u64>,
    },
    Restart {
        target: String,
        timeout_secs: Option<u64>,
    },
    Kill {
        target: String,
        signal: i32,
    },
    Rm {
        target: String,
        force: bool,
        keep_data: bool,
    },
    Wait {
        target: String,
    },
    Logs {
        target: String,
        tail: Option<usize>,
        offset: Option<u64>,
        #[serde(default)]
        source: LogSource,
        #[serde(default)]
        services: Vec<String>,
    },
    Exec {
        target: String,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        user: Option<String>,
        workdir: Option<String>,
        tty: bool,
        timeout_secs: Option<u64>,
        #[serde(default)]
        service: Option<String>,
    },
    ExecProbe {
        target: String,
        argv: Vec<String>,
        timeout_secs: u64,
    },
    Commit {
        target: String,
        image: String,
    },
    Ps {
        all: bool,
        project: Option<String>,
    },
    SecretEnsure {
        project: String,
        name: String,
        generate: String,
        length: usize,
    },
    SecretReveal {
        project: String,
        name: String,
    },
    SecretSet {
        project: String,
        name: String,
        value: String,
    },
    SecretList {
        project: Option<String>,
    },
    SecretRemove {
        project: String,
        name: String,
    },
    Stats {
        project: Option<String>,
    },
    LbSet {
        lb: LbSpec,
    },
    LbRemove {
        project: String,
        name: String,
    },
    LbList,
    Info,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Id { id: ContainerId },
    Containers(Vec<ContainerInfo>),
    Text { text: String },
    Names(Vec<String>),
    Exit { status: i32 },
    Log {
        data: String,
        next_offset: u64,
        #[serde(default)]
        source: LogSource,
    },
    Stats(Vec<ContainerStats>),
    Lbs(Vec<LbStatus>),
    Accepted,
    Error { code: i32, message: String },
}

impl Response {
    pub fn error(e: &Error) -> Response {
        Response::Error { code: e.exit_code(), message: e.payload() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    Starting,
    Healthy,
    Unhealthy,
}
