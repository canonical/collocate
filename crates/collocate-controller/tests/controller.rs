use collocate_compose::model::ComposeFile;
use collocate_compose::up::UpOptions;
use collocate_controller::controller::Controller;
use collocate_core::client::Api;
use collocate_core::request::{ContainerInfo, ContainerStats, HealthState, Request, Response, State};
use collocate_core::spec::Spec;
use collocate_core::{ContainerId, Error, Result};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

struct SimC {
    spec: Spec,
    state: State,
    born: u64,
    cpu_usec: u64,
}

#[derive(Default)]
struct Sim {
    now: u64,
    containers: Vec<SimC>,
    bad_revisions: HashSet<String>,
    load: HashMap<String, f64>,
    start_delay: u64,
    counter: u64,
    secrets: HashMap<String, String>,
    runs: Vec<String>,
}

impl Sim {
    fn new() -> Sim {
        Sim { start_delay: 3, ..Sim::default() }
    }

    fn advance(&mut self, secs: u64) {
        self.now += secs;
        for c in &mut self.containers {
            if c.state != State::Running {
                continue;
            }
            let svc = c.spec.labels.service.clone().unwrap_or_default();
            let load = self.load.get(&svc).copied().unwrap_or(0.0);
            let cores = f64::from(c.spec.limits.cpus_milli.unwrap_or(1000)) / 1000.0;
            c.cpu_usec += (load / 100.0 * cores * secs as f64 * 1e6) as u64;
        }
    }

    fn health(&self, c: &SimC) -> Option<HealthState> {
        c.spec.healthcheck.as_ref()?;
        let age = self.now - c.born;
        let bad = c.spec.labels.revision.as_ref().is_some_and(|r| self.bad_revisions.contains(r));
        Some(if bad {
            if age >= self.start_delay * 2 {
                HealthState::Unhealthy
            } else {
                HealthState::Starting
            }
        } else if age >= self.start_delay {
            HealthState::Healthy
        } else {
            HealthState::Starting
        })
    }

    fn ready(&self, service: &str, revision: Option<&str>) -> usize {
        self.containers
            .iter()
            .filter(|c| c.state == State::Running && c.spec.labels.service.as_deref() == Some(service))
            .filter(|c| revision.is_none_or(|r| c.spec.labels.revision.as_deref() == Some(r)))
            .filter(|c| self.health(c) == Some(HealthState::Healthy))
            .count()
    }

    fn running(&self, service: &str) -> usize {
        self.containers.iter().filter(|c| c.state == State::Running && c.spec.labels.service.as_deref() == Some(service)).count()
    }

    fn revisions(&self, service: &str) -> HashSet<String> {
        self.containers
            .iter()
            .filter(|c| c.spec.labels.service.as_deref() == Some(service))
            .filter_map(|c| c.spec.labels.revision.clone())
            .collect()
    }
}

impl Api for Sim {
    fn call(&mut self, req: Request) -> Result<Response> {
        match req {
            Request::Ps { all, project } => {
                let list = self
                    .containers
                    .iter()
                    .filter(|c| project.is_none() || c.spec.labels.project == project)
                    .filter(|c| all || c.state == State::Running)
                    .map(|c| ContainerInfo {
                        id: c.spec.id,
                        name: c.spec.name.clone(),
                        state: c.state,
                        pid: Some(1),
                        address: c.spec.net.addr,
                        project: c.spec.labels.project.clone(),
                        service: c.spec.labels.service.clone(),
                        health: self.health(c),
                        revision: c.spec.labels.revision.clone(),
                        series: None,
                        published: vec![],
                        image_kind: None,
                    })
                    .collect();
                Ok(Response::Containers(list))
            }
            Request::Stats { project } => Ok(Response::Stats(
                self.containers
                    .iter()
                    .filter(|c| c.state == State::Running && (project.is_none() || c.spec.labels.project == project))
                    .map(|c| ContainerStats {
                        id: c.spec.id,
                        name: c.spec.name.clone(),
                        project: c.spec.labels.project.clone(),
                        service: c.spec.labels.service.clone(),
                        cpu_usage_usec: c.cpu_usec,
                        memory_current: 10 << 20,
                        memory_max: c.spec.limits.memory,
                        pids: 2,
                        cpu_limit_milli: c.spec.limits.cpus_milli,
                    })
                    .collect(),
            )),
            Request::Run(spec) => {
                if self.containers.iter().any(|c| c.spec.name == spec.name) {
                    return Err(Error::Conflict(format!("name {} in use", spec.name)));
                }
                self.counter += 1;
                let mut s = *spec;
                s.id = ContainerId::from_bytes([(self.counter % 250) as u8 + 1, (self.counter / 250) as u8, 0, 0, 0, 0]);
                self.runs.push(s.name.clone());
                let id = s.id;
                self.containers.push(SimC { spec: s, state: State::Running, born: self.now, cpu_usec: 0 });
                Ok(Response::Id { id })
            }
            Request::Stop { target, .. } => {
                for c in &mut self.containers {
                    if c.spec.name == target {
                        c.state = State::Stopped;
                    }
                }
                Ok(Response::Ok)
            }
            Request::Rm { target, .. } => {
                self.containers.retain(|c| c.spec.name != target);
                Ok(Response::Ok)
            }
            Request::SecretEnsure { project, name, .. } => {
                self.secrets.entry(format!("{project}/{name}")).or_insert_with(|| "s".into());
                Ok(Response::Ok)
            }
            Request::SecretReveal { project, name } => {
                self.secrets.get(&format!("{project}/{name}")).map(|v| Response::Text { text: v.clone() }).ok_or(Error::NotFound(name))
            }
            Request::LbSet { .. } => Ok(Response::Ok),
            other => Err(Error::Internal(format!("sim does not implement {other:?}"))),
        }
    }
}

fn opts() -> UpOptions {
    UpOptions {
        subnet: "172.30.0.0/16".into(),
        base_dir: std::env::temp_dir(),
        regenerate_secrets: None,
        dry_run: false,
        ready_timeout: Duration::from_secs(1),
        poll_interval: Duration::from_millis(1),
    }
}

fn yaml(command: &str, extra: &str) -> String {
    format!(
        "version: 1\nproject: web\nservices:\n  api:\n    series: \"24.04\"\n    command: [\"{command}\"]\n    replicas: 3\n    memory: 128m\n    cpus: 1\n    healthcheck:\n      tcp: 8080\n    update:\n      max_surge: 1\n      max_unavailable: 0\n      min_ready: 2s\n      delay: 0s\n      progress_deadline: 120s\n{extra}"
    )
}

fn load(y: &str) -> ComposeFile {
    ComposeFile::load(y).unwrap()
}

fn step(c: &mut Controller, sim: &mut Sim) -> Vec<String> {
    sim.advance(1);
    let now = sim.now;
    c.tick(sim, now).unwrap()
}

fn settle(c: &mut Controller, sim: &mut Sim, max: u32) {
    for _ in 0..max {
        step(c, sim);
    }
}

#[test]
fn brings_up_the_declared_replicas() {
    let mut sim = Sim::new();
    let mut c = Controller::new(load(&yaml("/bin/api", "")), opts()).unwrap();
    settle(&mut c, &mut sim, 15);
    assert_eq!(sim.ready("api", None), 3);
    assert_eq!(sim.running("api"), 3);
    let mut names = sim.runs.clone();
    names.sort();
    assert_eq!(names, vec!["web-api-1", "web-api-2", "web-api-3"]);
}

#[test]
fn a_crashed_replica_is_replaced() {
    let mut sim = Sim::new();
    let mut c = Controller::new(load(&yaml("/bin/api", "")), opts()).unwrap();
    settle(&mut c, &mut sim, 15);
    sim.containers.iter_mut().find(|c| c.spec.name == "web-api-2").unwrap().state = State::Stopped;
    settle(&mut c, &mut sim, 15);
    assert_eq!(sim.ready("api", None), 3);
    assert_eq!(sim.runs.iter().filter(|n| *n == "web-api-2").count(), 2);
}

#[test]
fn a_steady_service_takes_no_actions() {
    let mut sim = Sim::new();
    let mut c = Controller::new(load(&yaml("/bin/api", "")), opts()).unwrap();
    settle(&mut c, &mut sim, 15);
    let runs = sim.runs.len();
    settle(&mut c, &mut sim, 20);
    assert_eq!(sim.runs.len(), runs);
}

#[test]
fn rolling_updates_never_reduce_availability_and_replace_every_replica() {
    let mut sim = Sim::new();
    let mut c = Controller::new(load(&yaml("/bin/api", "")), opts()).unwrap();
    settle(&mut c, &mut sim, 15);
    let old_revs = sim.revisions("api");
    assert_eq!(old_revs.len(), 1);
    c.set_file(load(&yaml("/bin/api-v2", "")));
    let mut events = Vec::new();
    for _ in 0..80 {
        events.extend(step(&mut c, &mut sim));
        assert!(sim.ready("api", None) >= 3, "availability dropped at t={}", sim.now);
        assert!(sim.running("api") <= 4, "surge exceeded at t={}", sim.now);
    }
    let revs = sim.revisions("api");
    assert_eq!(revs.len(), 1);
    assert!(revs.is_disjoint(&old_revs));
    assert_eq!(sim.ready("api", None), 3);
    assert!(events.iter().any(|e| e.contains("rollout")), "{events:?}");
}

#[test]
fn a_bad_revision_is_rolled_back_without_losing_availability() {
    let mut sim = Sim::new();
    let mut c = Controller::new(load(&yaml("/bin/api", "")), opts()).unwrap();
    settle(&mut c, &mut sim, 15);
    let good = sim.revisions("api");
    let bad_file = load(&yaml("/bin/api-broken", ""));
    let mut probe = Sim::new();
    let mut probe_ctrl = Controller::new(bad_file.clone(), opts()).unwrap();
    settle(&mut probe_ctrl, &mut probe, 2);
    let bad_rev = probe.revisions("api").into_iter().next().unwrap();
    sim.bad_revisions.insert(bad_rev.clone());
    c.set_file(bad_file);
    let mut events = Vec::new();
    for _ in 0..120 {
        events.extend(step(&mut c, &mut sim));
        assert!(sim.ready("api", None) >= 3, "availability dropped at t={}", sim.now);
    }
    assert_eq!(sim.revisions("api"), good);
    assert_eq!(sim.ready("api", None), 3);
    assert!(events.iter().any(|e| e.contains("rollback")), "{events:?}");
    let before = sim.runs.len();
    settle(&mut c, &mut sim, 30);
    assert_eq!(sim.runs.len(), before, "the rejected revision must not be retried");
}

const AUTOSCALE: &str = "    autoscale:\n      min: 1\n      max: 4\n      metrics:\n        - type: cpu\n          target: 50\n      up:\n        stabilization: 5s\n        max_step: 4\n      down:\n        stabilization: 20s\n        max_step: 1\n";

#[test]
fn autoscaling_follows_cpu_load_up_and_back_down() {
    let mut sim = Sim::new();
    let y = yaml("/bin/api", AUTOSCALE).replace("replicas: 3", "replicas: 1");
    let mut c = Controller::new(load(&y), opts()).unwrap();
    settle(&mut c, &mut sim, 10);
    assert_eq!(sim.running("api"), 1);

    sim.load.insert("api".into(), 200.0);
    settle(&mut c, &mut sim, 60);
    assert_eq!(sim.running("api"), 4, "should scale to the maximum under sustained load");

    sim.load.insert("api".into(), 5.0);
    settle(&mut c, &mut sim, 150);
    assert_eq!(sim.running("api"), 1, "should scale back down once load subsides");
    assert!(sim.ready("api", None) >= 1);
}

#[test]
fn autoscaling_holds_steady_near_the_target() {
    let mut sim = Sim::new();
    let y = yaml("/bin/api", AUTOSCALE).replace("replicas: 3", "replicas: 2");
    sim.load.insert("api".into(), 52.0);
    let mut c = Controller::new(load(&y), opts()).unwrap();
    settle(&mut c, &mut sim, 10);
    let before = sim.runs.len();
    settle(&mut c, &mut sim, 60);
    assert_eq!(sim.runs.len(), before);
    assert_eq!(sim.running("api"), 2);
}

#[test]
fn paused_services_are_left_alone() {
    let mut sim = Sim::new();
    let mut c = Controller::new(load(&yaml("/bin/api", "")), opts()).unwrap();
    settle(&mut c, &mut sim, 15);
    c.pause("api");
    sim.containers.iter_mut().find(|c| c.spec.name == "web-api-1").unwrap().state = State::Stopped;
    let runs = sim.runs.len();
    settle(&mut c, &mut sim, 10);
    assert_eq!(sim.runs.len(), runs);
    c.resume("api");
    settle(&mut c, &mut sim, 10);
    assert!(sim.runs.len() > runs);
}
