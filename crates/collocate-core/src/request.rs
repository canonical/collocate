use crate::auth::{Access, Caller, Role, Scope};
use crate::id::ContainerId;
use crate::net::{Algorithm, NoBackends, Proto};
use crate::settings::DaemonSettings;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryCredential {
    pub registry: String,
    pub username: String,
    pub password: String,
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
    Init {
        settings: DaemonSettings,
        #[serde(default)]
        force: bool,
    },
    ImagePull {
        reference: String,
        policy: String,
        #[serde(default)]
        credentials: Vec<RegistryCredential>,
    },
    ImageImport,
    ImageList,
    ImageShow {
        name: String,
    },
    ImageDelete {
        name: String,
    },
    ImagePrune,
    ControllerSet {
        compose: String,
    },
    ConfigPut {
        project: String,
        name: String,
        content: String,
    },
    As {
        caller: Caller,
        request: Box<Request>,
    },
    TrustTokenCreate {
        name: String,
        role: Role,
        #[serde(default)]
        projects: Vec<String>,
        #[serde(default)]
        expiry_secs: Option<u64>,
    },
    TrustTokenList,
    TrustTokenRevoke {
        name: String,
    },
    TrustList,
    TrustRemove {
        name: String,
    },
    TrustAddCertificate {
        name: String,
        certificate: String,
        role: Role,
        #[serde(default)]
        projects: Vec<String>,
    },
    TrustEnroll {
        secret: String,
        certificate: String,
        #[serde(default)]
        name: Option<String>,
    },
    TrustLookup {
        fingerprint: String,
    },
}

impl Request {
    pub fn allowed_uninitialized(&self) -> bool {
        matches!(self, Request::Info | Request::Init { .. } | Request::Shutdown)
    }

    pub fn verb(&self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v["verb"].as_str().map(String::from)).unwrap_or_default()
    }

    pub fn needs_descriptors(&self) -> bool {
        matches!(self, Request::Exec { .. } | Request::ImageImport)
    }

    pub fn access(&self) -> Access {
        use Role::{Admin, Operator, Viewer};
        let target = |t: &String| Scope::Target(t.clone());
        match self {
            Request::Info => Access::new(Viewer, Scope::Open),
            Request::Ps { project, .. } | Request::Stats { project } => Access::new(Viewer, Scope::Filtered(project.clone())),
            Request::SecretList { project } => Access::new(Viewer, Scope::Filtered(project.clone())),
            Request::LbList => Access::new(Viewer, Scope::Filtered(None)),
            Request::Logs { target: t, .. } | Request::Wait { target: t } => Access::new(Viewer, target(t)),
            Request::ImageList | Request::ImageShow { .. } => Access::new(Viewer, Scope::Open),
            Request::Run(spec) => Access::new(Operator, Scope::Project(spec.labels.project.clone())),
            Request::Start { target: t }
            | Request::Stop { target: t, .. }
            | Request::Restart { target: t, .. }
            | Request::Kill { target: t, .. }
            | Request::Rm { target: t, .. }
            | Request::Exec { target: t, .. }
            | Request::ExecProbe { target: t, .. }
            | Request::Commit { target: t, .. } => Access::new(Operator, target(t)),
            Request::SecretEnsure { project, .. }
            | Request::SecretReveal { project, .. }
            | Request::SecretSet { project, .. }
            | Request::SecretRemove { project, .. }
            | Request::LbRemove { project, .. }
            | Request::ConfigPut { project, .. } => Access::new(Operator, Scope::Project(Some(project.clone()))),
            Request::LbSet { lb } => Access::new(Operator, Scope::Project(Some(lb.project.clone()))),
            Request::ImagePull { .. } | Request::ImageImport => Access::new(Operator, Scope::Open),
            Request::ImageDelete { .. } | Request::ImagePrune => Access::new(Operator, Scope::Unrestricted),
            Request::Init { .. }
            | Request::Shutdown
            | Request::ControllerSet { .. }
            | Request::TrustTokenCreate { .. }
            | Request::TrustTokenList
            | Request::TrustTokenRevoke { .. }
            | Request::TrustList
            | Request::TrustRemove { .. }
            | Request::TrustAddCertificate { .. } => Access::new(Admin, Scope::Unrestricted),
            Request::As { .. } | Request::TrustEnroll { .. } | Request::TrustLookup { .. } => Access::new(Admin, Scope::Internal),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Id {
        id: ContainerId,
    },
    Containers(Vec<ContainerInfo>),
    Text {
        text: String,
    },
    Names(Vec<String>),
    Exit {
        status: i32,
    },
    Log {
        data: String,
        next_offset: u64,
        #[serde(default)]
        source: LogSource,
    },
    Stats(Vec<ContainerStats>),
    Lbs(Vec<LbStatus>),
    Accepted,
    Json {
        value: serde_json::Value,
    },
    Error {
        code: i32,
        message: String,
    },
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
