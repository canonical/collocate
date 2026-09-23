use crate::plan::topo_order;
use crate::template::{references, Reference};
use collocate_core::net::Publish;
use collocate_core::policy::{duration_secs, Autoscale, UpdatePolicy};
use collocate_core::size::{parse_duration_secs, parse_size};
use collocate_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SUPPORTED_VERSION: u32 = 1;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::InvalidSpec(msg.into()))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDef {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub cpus: Option<f64>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretDef {
    #[serde(default)]
    pub generate: Option<String>,
    #[serde(default)]
    pub length: Option<usize>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub per_node: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDef {
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDef {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpCheck {
    pub port: u16,
    #[serde(default = "root_path")]
    pub path: String,
}

fn root_path() -> String {
    "/".into()
}

fn thirty() -> String {
    "30s".into()
}

fn five() -> String {
    "5s".into()
}

fn zero() -> String {
    "0s".into()
}

fn three() -> u32 {
    3
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthDef {
    #[serde(default)]
    pub tcp: Option<u16>,
    #[serde(default)]
    pub http: Option<HttpCheck>,
    #[serde(default)]
    pub exec: Option<Vec<String>>,
    #[serde(default = "thirty")]
    pub interval: String,
    #[serde(default = "five")]
    pub timeout: String,
    #[serde(default = "three")]
    pub retries: u32,
    #[serde(default = "zero")]
    pub start_period: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum DepRaw {
    Name(String),
    Full {
        service: String,
        #[serde(default)]
        healthcheck: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "DepRaw")]
pub struct Dependency {
    pub service: String,
    pub healthcheck: Vec<String>,
}

impl From<DepRaw> for Dependency {
    fn from(raw: DepRaw) -> Self {
        match raw {
            DepRaw::Name(service) => Dependency { service, healthcheck: Vec::new() },
            DepRaw::Full { service, healthcheck } => Dependency { service, healthcheck },
        }
    }
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub cpus: Option<f64>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub configs: BTreeMap<String, String>,
    #[serde(default)]
    pub depends_on: Vec<Dependency>,
    #[serde(default = "one")]
    pub replicas: u32,
    #[serde(default)]
    pub healthcheck: Option<HealthDef>,
    #[serde(default)]
    pub autoscale: Option<Autoscale>,
    #[serde(default)]
    pub update: Option<UpdatePolicy>,
    #[serde(default)]
    pub restart: Option<String>,
    #[serde(default)]
    pub tmpfs: Vec<String>,
    #[serde(default)]
    pub stop_signal: Option<String>,
    #[serde(default)]
    pub stop_timeout: Option<u64>,
    #[serde(default)]
    pub ulimits: BTreeMap<String, u64>,
    #[serde(default)]
    pub cap_add: Vec<String>,
    #[serde(default)]
    pub cap_drop: Vec<String>,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
}

impl Default for Service {
    fn default() -> Self {
        Service {
            node: None,
            image: None,
            series: None,
            persistent: false,
            cpus: None,
            memory: None,
            volume: None,
            volumes: Vec::new(),
            publish: Vec::new(),
            command: Vec::new(),
            entrypoint: Vec::new(),
            env: BTreeMap::new(),
            secrets: Vec::new(),
            configs: BTreeMap::new(),
            depends_on: Vec::new(),
            replicas: 1,
            healthcheck: None,
            autoscale: None,
            update: None,
            restart: None,
            tmpfs: Vec::new(),
            stop_signal: None,
            stop_timeout: None,
            ulimits: BTreeMap::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            read_only: false,
            user: None,
            workdir: None,
            hostname: None,
        }
    }
}

impl Service {
    pub fn max_replicas(&self) -> u32 {
        self.autoscale.as_ref().map_or(self.replicas, |a| a.max.max(self.replicas))
    }

    pub fn index_bound(&self) -> u32 {
        self.max_replicas() + self.update.map_or(UpdatePolicy::default().max_surge, |u| u.max_surge)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRef {
    pub service: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LbType {
    L4,
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LbAlgorithm {
    RoundRobin,
    Random,
    SourceHash,
}

fn thirty_secs() -> u64 {
    30
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LbDef {
    #[serde(rename = "type", default = "l4")]
    pub kind: LbType,
    pub listen: u16,
    #[serde(default)]
    pub publish: Vec<String>,
    pub backends: BackendRef,
    #[serde(default = "round_robin")]
    pub algorithm: LbAlgorithm,
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default = "thirty_secs", rename = "drain", deserialize_with = "duration_secs")]
    pub drain_secs: u64,
    #[serde(default)]
    pub on_no_backends: Option<String>,
}

fn l4() -> LbType {
    LbType::L4
}

fn round_robin() -> LbAlgorithm {
    LbAlgorithm::RoundRobin
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposeFile {
    pub version: u32,
    pub project: String,
    #[serde(default)]
    pub nodes: BTreeMap<String, NodeDef>,
    #[serde(default)]
    pub services: BTreeMap<String, Service>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretDef>,
    #[serde(default)]
    pub configs: BTreeMap<String, ConfigDef>,
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryDef>,
    #[serde(default)]
    pub loadbalancers: BTreeMap<String, LbDef>,
}

impl ComposeFile {
    pub fn parse(yaml: &str) -> Result<Self> {
        serde_saphyr::from_str(yaml).map_err(|e| Error::Parse(e.to_string()))
    }

    pub fn load(yaml: &str) -> Result<Self> {
        let f = Self::parse(yaml)?;
        f.validate()?;
        Ok(f)
    }

    pub fn dependency_graph(&self) -> BTreeMap<String, Vec<String>> {
        self.services.iter().map(|(n, s)| (n.clone(), s.depends_on.iter().map(|d| d.service.clone()).collect())).collect()
    }

    fn check_refs(&self, owner: &str, text: &str) -> Result<()> {
        for r in references(text)? {
            match r {
                Reference::Secret(n) if !self.secrets.contains_key(&n) => {
                    return invalid(format!("{owner} references undeclared secret {n}"));
                }
                Reference::ServiceAddress(n) | Reference::ServiceAddresses(n) if !self.services.contains_key(&n) => {
                    return invalid(format!("{owner} references unknown service {n}"));
                }
                Reference::LbAddress(n) if !self.loadbalancers.contains_key(&n) => {
                    return invalid(format!("{owner} references unknown load balancer {n}"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != SUPPORTED_VERSION {
            return invalid(format!("unsupported version {}", self.version));
        }
        let mut published: BTreeSet<(Option<String>, u16, String)> = BTreeSet::new();
        for (name, s) in &self.services {
            if s.image.is_some() == s.series.is_some() {
                return invalid(format!("service {name} needs exactly one of series or image"));
            }
            if let Some(series) = &s.series {
                collocate_core::spec::Series::parse(series)?;
            }
            if s.replicas == 0 {
                return invalid(format!("service {name}: replicas must be at least 1"));
            }
            if let Some(n) = &s.node {
                if !self.nodes.contains_key(n) {
                    return invalid(format!("service {name} uses unknown node {n}"));
                }
            }
            for d in &s.depends_on {
                if !self.services.contains_key(&d.service) {
                    return invalid(format!("service {name} depends on unknown service {}", d.service));
                }
            }
            for sec in &s.secrets {
                if !self.secrets.contains_key(sec) {
                    return invalid(format!("service {name} uses undeclared secret {sec}"));
                }
            }
            for cfg in s.configs.keys() {
                if !self.configs.contains_key(cfg) {
                    return invalid(format!("service {name} uses undeclared config {cfg}"));
                }
            }
            for text in s.env.values().chain(&s.command).chain(&s.entrypoint) {
                self.check_refs(&format!("service {name}"), text)?;
            }
            if let Some(m) = &s.memory {
                parse_size(m)?;
            }
            for p in &s.publish {
                let p = Publish::parse(p)?;
                if !published.insert((s.node.clone(), p.host, p.proto.to_string())) {
                    return invalid(format!("host port {} is published twice", p.host));
                }
            }
            if let Some(h) = &s.healthcheck {
                for d in [&h.interval, &h.timeout, &h.start_period] {
                    parse_duration_secs(d)?;
                }
                if [h.tcp.is_some(), h.http.is_some(), h.exec.is_some()].iter().filter(|b| **b).count() != 1 {
                    return invalid(format!("service {name}: healthcheck needs exactly one of tcp, http or exec"));
                }
            }
            if let Some(r) = &s.restart {
                let ok = matches!(r.as_str(), "no" | "always" | "on-failure")
                    || r.strip_prefix("on-failure:").is_some_and(|n| n.parse::<u32>().is_ok());
                if !ok {
                    return invalid(format!("service {name}: invalid restart policy {r}"));
                }
            }
            if let Some(a) = &s.autoscale {
                if s.persistent {
                    return invalid(format!("service {name}: autoscale is not allowed on persistent services"));
                }
                if a.metrics.is_empty() {
                    return invalid(format!("service {name}: autoscale needs metrics"));
                }
                if a.min == 0 || a.min > a.max {
                    return invalid(format!("service {name}: autoscale min must be within 1..=max"));
                }
                if s.memory.is_none() {
                    return invalid(format!("service {name}: autoscale requires a memory limit"));
                }
            }
        }
        let mut listens = BTreeSet::new();
        for (name, lb) in &self.loadbalancers {
            if !self.services.contains_key(&lb.backends.service) {
                return invalid(format!("load balancer {name} targets unknown service {}", lb.backends.service));
            }
            if !listens.insert(lb.listen) {
                return invalid(format!("duplicate listen port {} across load balancers", lb.listen));
            }
            for p in &lb.publish {
                let p = Publish::parse(p)?;
                if !published.insert((None, p.host, p.proto.to_string())) {
                    return invalid(format!("host port {} is published twice", p.host));
                }
            }
        }
        for (host, reg) in &self.registries {
            self.check_refs(&format!("registry {host}"), &reg.username)?;
            self.check_refs(&format!("registry {host}"), &reg.password)?;
        }
        topo_order(&self.dependency_graph())?;
        Ok(())
    }
}
