#![allow(dead_code)]
use collocate_core::client::Api;
use collocate_core::request::{ContainerInfo, HealthState, Request, Response, State};
use collocate_core::spec::Spec;
use collocate_core::{ContainerId, Error, Result};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub struct FakeDaemon {
    pub containers: Vec<(Spec, State)>,
    pub secrets: BTreeMap<String, String>,
    pub lbs: Vec<collocate_core::request::LbSpec>,
    pub log: Vec<String>,
    pub health_after_polls: HashMap<String, u32>,
    pub polls: HashMap<String, u32>,
    pub probe_ready_after: HashMap<String, u32>,
    pub probe_polls: HashMap<String, u32>,
    pub fail_run: Option<String>,
    counter: u8,
}

impl FakeDaemon {
    pub fn running_names(&self) -> Vec<String> {
        self.containers.iter().filter(|(_, s)| *s == State::Running).map(|(c, _)| c.name.clone()).collect()
    }

    fn info(&mut self, spec: &Spec, state: State) -> ContainerInfo {
        let polls = self.polls.entry(spec.name.clone()).or_insert(0);
        *polls += 1;
        let need = self.health_after_polls.get(&spec.name).copied().unwrap_or(0);
        let health = spec.healthcheck.as_ref().map(|_| if *polls > need { HealthState::Healthy } else { HealthState::Starting });
        ContainerInfo {
            id: spec.id,
            name: spec.name.clone(),
            state,
            pid: Some(1),
            address: spec.net.addr,
            project: spec.labels.project.clone(),
            service: spec.labels.service.clone(),
            health,
            revision: spec.labels.revision.clone(),
            series: None,
            published: vec![],
            image_kind: None,
        }
    }
}

impl Api for FakeDaemon {
    fn call(&mut self, req: Request) -> Result<Response> {
        match req {
            Request::Ps { all, project } => {
                let items: Vec<(Spec, State)> = self.containers.clone();
                let mut out = Vec::new();
                for (s, st) in items {
                    if project.is_some() && s.labels.project != project {
                        continue;
                    }
                    if !all && st != State::Running {
                        continue;
                    }
                    out.push(self.info(&s, st));
                }
                Ok(Response::Containers(out))
            }
            Request::Run(spec) => {
                if let Some(msg) = &self.fail_run {
                    return Err(Error::Internal(msg.clone()));
                }
                if self.containers.iter().any(|(c, _)| c.name == spec.name) {
                    return Err(Error::Conflict(format!("name {} in use", spec.name)));
                }
                self.counter += 1;
                let mut s = *spec;
                s.id = ContainerId::from_bytes([self.counter; 6]);
                self.log.push(format!("run {}", s.name));
                self.containers.push((s.clone(), State::Running));
                Ok(Response::Id { id: s.id })
            }
            Request::Stop { target, .. } => {
                self.log.push(format!("stop {target}"));
                for (c, st) in &mut self.containers {
                    if c.name == target {
                        *st = State::Stopped;
                    }
                }
                Ok(Response::Ok)
            }
            Request::Start { target } => {
                self.log.push(format!("start {target}"));
                for (c, st) in &mut self.containers {
                    if c.name == target {
                        *st = State::Running;
                    }
                }
                Ok(Response::Ok)
            }
            Request::Rm { target, .. } => {
                self.log.push(format!("rm {target}"));
                self.containers.retain(|(c, _)| c.name != target);
                Ok(Response::Ok)
            }
            Request::SecretEnsure { project, name, .. } => {
                self.secrets.entry(format!("{project}/{name}")).or_insert_with(|| format!("generated-{name}"));
                self.log.push(format!("secret-ensure {name}"));
                Ok(Response::Ok)
            }
            Request::SecretSet { project, name, value } => {
                self.secrets.insert(format!("{project}/{name}"), value);
                self.log.push(format!("secret-set {name}"));
                Ok(Response::Ok)
            }
            Request::SecretRemove { project, name } => {
                self.secrets.remove(&format!("{project}/{name}"));
                self.log.push(format!("secret-rm {name}"));
                Ok(Response::Ok)
            }
            Request::SecretReveal { project, name } => match self.secrets.get(&format!("{project}/{name}")) {
                Some(v) => Ok(Response::Text { text: v.clone() }),
                None => Err(Error::NotFound(name)),
            },
            Request::LbSet { lb } => {
                self.log.push(format!("lb-set {}", lb.name));
                self.lbs.retain(|l| l.name != lb.name);
                self.lbs.push(lb);
                Ok(Response::Ok)
            }
            Request::LbRemove { name, .. } => {
                self.log.push(format!("lb-rm {name}"));
                self.lbs.retain(|l| l.name != name);
                Ok(Response::Ok)
            }
            Request::ExecProbe { target, .. } => {
                let polls = self.probe_polls.entry(target.clone()).or_insert(0);
                *polls += 1;
                let need = self.probe_ready_after.get(&target).copied().unwrap_or(0);
                Ok(Response::Exit { status: if *polls > need { 0 } else { 1 } })
            }
            other => Err(Error::Internal(format!("fake daemon does not implement {other:?}"))),
        }
    }
}
