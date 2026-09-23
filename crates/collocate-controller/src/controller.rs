use crate::autoscaler::{Decision, MetricReading, Scaler};
use crate::rollout::{next_step, Action, Health, Lifecycle, Replica, State};
use collocate_compose::model::{ComposeFile, Service};
use collocate_compose::plan::topo_order;
use collocate_compose::up::{BuildState, UpOptions};
use collocate_core::client::Api;
use collocate_core::policy::MetricKind;
use collocate_core::request::{ContainerInfo, ContainerStats, HealthState, Request, Response, State as RunState};
use collocate_core::{ContainerId, Error, Result};
use std::collections::{HashMap, HashSet};

const CRASH_LIMIT: u32 = 3;
const NEVER: u64 = 1 << 40;

#[derive(Clone)]
struct Applied {
    revision: String,
    service: Service,
}

pub struct Controller {
    file: ComposeFile,
    opts: UpOptions,
    applied: HashMap<String, Applied>,
    previous: HashMap<String, Applied>,
    rejected: HashMap<String, String>,
    scalers: HashMap<String, Scaler>,
    desired: HashMap<String, u32>,
    healthy_since: HashMap<String, u64>,
    rollout_start: HashMap<String, u64>,
    last_action: HashMap<String, u64>,
    prev_cpu: HashMap<ContainerId, (u64, u64)>,
    paused: HashSet<String>,
    crashes: HashMap<String, u32>,
}

fn replica_index(project: &str, service: &str, name: &str) -> Option<u32> {
    name.strip_prefix(&format!("{project}-{service}-"))?.parse::<u32>().ok()?.checked_sub(1)
}

fn unexpected<T>(r: Response) -> Result<T> {
    Err(Error::Internal(format!("unexpected response {r:?}")))
}

impl Controller {
    pub fn new(file: ComposeFile, opts: UpOptions) -> Result<Controller> {
        file.validate()?;
        Ok(Controller {
            file,
            opts,
            applied: HashMap::new(),
            previous: HashMap::new(),
            rejected: HashMap::new(),
            scalers: HashMap::new(),
            desired: HashMap::new(),
            healthy_since: HashMap::new(),
            rollout_start: HashMap::new(),
            last_action: HashMap::new(),
            prev_cpu: HashMap::new(),
            paused: HashSet::new(),
            crashes: HashMap::new(),
        })
    }

    pub fn set_file(&mut self, file: ComposeFile) {
        for (name, svc) in &file.services {
            let changed = self.file.services.get(name).is_none_or(|old| old.autoscale != svc.autoscale);
            if changed {
                self.scalers.remove(name);
                self.desired.remove(name);
            }
        }
        self.file = file;
    }

    pub fn pause(&mut self, service: &str) {
        self.paused.insert(service.to_string());
    }

    pub fn resume(&mut self, service: &str) {
        self.paused.remove(service);
    }

    fn effective_file(&self, service: &str) -> ComposeFile {
        let mut f = self.file.clone();
        if let Some(a) = self.applied.get(service) {
            f.services.insert(service.to_string(), a.service.clone());
        }
        f
    }

    fn observe(&mut self, api: &mut dyn Api, service: &str, revision: &str, now: u64, events: &mut Vec<String>) -> Result<Vec<Replica>> {
        let project = self.file.project.clone();
        let list = match api.call(Request::Ps { all: true, project: Some(project.clone()) })? {
            Response::Containers(c) => c,
            other => return unexpected(other),
        };
        let mut out = Vec::new();
        for c in list.iter().filter(|c| c.service.as_deref() == Some(service)) {
            let Some(index) = replica_index(&project, service, &c.name) else { continue };
            let is_target = c.revision.as_deref() == Some(revision);
            if c.state != RunState::Running {
                if is_target {
                    *self.crashes.entry(service.to_string()).or_insert(0) += 1;
                    events.push(format!("{service}: replica {} exited, replacing it", c.name));
                }
                api.call(Request::Rm { target: c.name.clone(), force: true, keep_data: false })?;
                self.healthy_since.remove(&c.name);
                continue;
            }
            let health = match c.health {
                None => {
                    let since = *self.healthy_since.entry(c.name.clone()).or_insert(now);
                    Health::Healthy { for_secs: now.saturating_sub(since) }
                }
                Some(HealthState::Healthy) => {
                    let since = *self.healthy_since.entry(c.name.clone()).or_insert(now);
                    Health::Healthy { for_secs: now.saturating_sub(since) }
                }
                Some(HealthState::Starting) => {
                    self.healthy_since.remove(&c.name);
                    Health::Starting
                }
                Some(HealthState::Unhealthy) => {
                    self.healthy_since.remove(&c.name);
                    Health::Unhealthy
                }
            };
            out.push(Replica {
                index,
                revision: c.revision.clone().unwrap_or_default(),
                health,
                lifecycle: Lifecycle::Live,
                failed: is_target && c.health == Some(HealthState::Unhealthy),
                connections: 0,
            });
        }
        if self.crashes.get(service).copied().unwrap_or(0) >= CRASH_LIMIT && out.iter().any(|r| r.revision != revision) {
            out.push(Replica {
                index: u32::MAX,
                revision: revision.to_string(),
                health: Health::Unhealthy,
                lifecycle: Lifecycle::Live,
                failed: true,
                connections: 0,
            });
        }
        Ok(out)
    }

    fn readings(&mut self, stats: &[ContainerStats], service: &str, now: u64) -> Vec<MetricReading> {
        let mine: Vec<&ContainerStats> = stats.iter().filter(|s| s.service.as_deref() == Some(service)).collect();
        let (mut cpu, mut cpu_n, mut mem, mut mem_n) = (0.0, 0, 0.0, 0);
        for s in &mine {
            if let Some((usec, at)) = self.prev_cpu.get(&s.id).copied() {
                let dt = now.saturating_sub(at) as f64;
                if dt > 0.0 {
                    let cores = f64::from(s.cpu_limit_milli.unwrap_or(1000)) / 1000.0;
                    cpu += (s.cpu_usage_usec.saturating_sub(usec) as f64 / (dt * 1e6)) / cores * 100.0;
                    cpu_n += 1;
                }
            }
            self.prev_cpu.insert(s.id, (s.cpu_usage_usec, now));
            if let Some(max) = s.memory_max {
                mem += s.memory_current as f64 / max as f64 * 100.0;
                mem_n += 1;
            }
        }
        let mut out = Vec::new();
        if cpu_n > 0 {
            out.push(MetricReading { kind: MetricKind::Cpu, value: cpu / f64::from(cpu_n) });
        }
        if mem_n > 0 {
            out.push(MetricReading { kind: MetricKind::Memory, value: mem / f64::from(mem_n) });
        }
        out
    }

    pub fn tick(&mut self, api: &mut dyn Api, now: u64) -> Result<Vec<String>> {
        let mut events = Vec::new();
        let mut state = BuildState::new(api, &self.file, &self.opts)?;
        let order = topo_order(&self.file.dependency_graph())?;
        let stats = match api.call(Request::Stats { project: Some(self.file.project.clone()) }) {
            Ok(Response::Stats(s)) => s,
            _ => Vec::new(),
        };
        for svc in order {
            let file_def = self.file.services[&svc].clone();
            let file_rev = state.spec(&self.file, &self.opts, &svc, 0)?.labels.revision.unwrap_or_default();
            let rejected = self.rejected.get(&svc).is_some_and(|r| *r == file_rev);
            match self.applied.get(&svc) {
                None => {
                    self.applied.insert(svc.clone(), Applied { revision: file_rev.clone(), service: file_def.clone() });
                }
                Some(cur) if cur.revision != file_rev && !rejected => {
                    self.previous.insert(svc.clone(), cur.clone());
                    self.applied.insert(svc.clone(), Applied { revision: file_rev.clone(), service: file_def.clone() });
                    self.rollout_start.insert(svc.clone(), now);
                    self.crashes.remove(&svc);
                    events.push(format!("{svc}: rollout to revision {file_rev} started"));
                }
                _ => {}
            }
            if self.paused.contains(&svc) {
                continue;
            }
            let applied = self.applied[&svc].clone();
            let replicas = self.observe(api, &svc, &applied.revision, now, &mut events)?;
            let rolling = replicas.iter().any(|r| r.revision != applied.revision);

            let mut desired = self.desired.get(&svc).copied().unwrap_or(file_def.replicas);
            if let (Some(policy), false) = (file_def.autoscale.clone(), rolling) {
                let readings = self.readings(&stats, &svc, now);
                let scaler = self.scalers.entry(svc.clone()).or_insert_with(|| Scaler::new(policy));
                if let Decision::Scale { to } = scaler.decide(now, desired, &readings) {
                    if to != desired {
                        events.push(format!("{svc}: autoscaling {desired} -> {to}"));
                    }
                    desired = to;
                }
                self.desired.insert(svc.clone(), desired);
            }

            let st = State {
                desired,
                target: applied.revision.clone(),
                previous: self.previous.get(&svc).map(|p| p.revision.clone()),
                policy: applied.service.update.unwrap_or_default(),
                drain_secs: 0,
                replicas,
                elapsed_secs: now.saturating_sub(self.rollout_start.get(&svc).copied().unwrap_or(now)),
                since_last_action_secs: now.saturating_sub(self.last_action.get(&svc).copied().unwrap_or(now.saturating_sub(NEVER))),
                paused: false,
            };
            for action in next_step(&st) {
                match action {
                    Action::Start { index, revision } => {
                        let eff = self.effective_file(&svc);
                        let spec = state.spec(&eff, &self.opts, &svc, index)?;
                        if spec.labels.revision.as_deref() != Some(revision.as_str()) {
                            continue;
                        }
                        let name = spec.name.clone();
                        match api.call(Request::Run(Box::new(spec))) {
                            Ok(_) => {
                                events.push(format!("{svc}: started {name}"));
                                self.last_action.insert(svc.clone(), now);
                            }
                            Err(e) => events.push(format!("{svc}: cannot start {name}: {e}")),
                        }
                    }
                    Action::Drain { index } => {
                        let name = format!("{}-{}-{}", self.file.project, svc, index + 1);
                        api.call(Request::Stop { target: name.clone(), timeout_secs: None })?;
                        api.call(Request::Rm { target: name.clone(), force: true, keep_data: false })?;
                        self.healthy_since.remove(&name);
                        events.push(format!("{svc}: removed {name}"));
                        self.last_action.insert(svc.clone(), now);
                    }
                    Action::Remove { index } => {
                        let name = format!("{}-{}-{}", self.file.project, svc, index + 1);
                        api.call(Request::Rm { target: name, force: true, keep_data: false })?;
                    }
                    Action::Rollback { to } => {
                        if let Some(prev) = self.previous.remove(&svc) {
                            events.push(format!("{svc}: rollback to revision {to}"));
                            self.rejected.insert(svc.clone(), applied.revision.clone());
                            self.applied.insert(svc.clone(), prev);
                            self.rollout_start.insert(svc.clone(), now);
                            self.crashes.remove(&svc);
                        }
                    }
                    Action::Pause { reason } => {
                        events.push(format!("{svc}: paused ({reason})"));
                        self.paused.insert(svc.clone());
                    }
                    Action::Complete => {
                        if self.rollout_start.remove(&svc).is_some() {
                            events.push(format!("{svc}: rollout complete"));
                        }
                        self.crashes.remove(&svc);
                    }
                }
            }
        }
        let _ = ContainerInfo::clone;
        Ok(events)
    }
}
