use crate::build::{build_spec, BuildCtx};
use crate::model::{ComposeFile, RegistryDef};
use crate::plan::{diff, shutdown_order, topo_order, Actual, Item, Plan};
use crate::template::{references, resolve, Context, Reference};
use collocate_core::client::Api;
use collocate_core::net::Proto;
use collocate_core::request::{ContainerInfo, HealthState, LbSpec, Request, Response, State};
use collocate_core::spec::{Series, Spec};
use collocate_core::{Error, Result};
use collocate_net::ipam::{Ipam, Subnet};
use collocate_image::config::ImageMeta;
use collocate_image::pull::PullPolicy;
use collocate_registry::auth::Credentials;
use collocate_registry::Reference as OciReference;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct ComposeCredentials<'a> {
    registries: &'a BTreeMap<String, RegistryDef>,
    tctx: &'a Context,
}

impl Credentials for ComposeCredentials<'_> {
    fn for_registry(&self, registry: &str) -> Option<(String, String)> {
        let def = self.registries.get(registry)?;
        let user = resolve(&def.username, self.tctx).ok()?;
        let pass = resolve(&def.password, self.tctx).ok()?;
        Some((user, pass))
    }
}

#[derive(Debug, Clone)]
pub struct UpOptions {
    pub subnet: String,
    pub base_dir: PathBuf,
    pub state_dir: PathBuf,
    pub regenerate_secrets: Option<Vec<String>>,
    pub dry_run: bool,
    pub ready_timeout: Duration,
    pub poll_interval: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UpReport {
    pub created: Vec<String>,
    pub recreated: Vec<String>,
    pub started: Vec<String>,
    pub removed: Vec<String>,
    pub kept: Vec<String>,
}

fn containers(api: &mut dyn Api, project: &str) -> Result<Vec<ContainerInfo>> {
    match api.call(Request::Ps { all: true, project: Some(project.to_string()) })? {
        Response::Containers(c) => Ok(c),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn replica_index(project: &str, service: &str, name: &str) -> Option<u32> {
    name.strip_prefix(&format!("{project}-{service}-"))?.parse::<u32>().ok()?.checked_sub(1)
}

struct Prepared {
    order: Vec<String>,
    specs: BTreeMap<String, Vec<Spec>>,
    lb_vips: HashMap<String, Ipv4Addr>,
    forced: HashSet<String>,
}

fn secret_value_from_file(file: &ComposeFile, name: &str, opts: &UpOptions) -> Result<String> {
    let def = &file.secrets[name];
    let path = def.file.as_ref().ok_or_else(|| Error::InvalidSpec(format!("secret {name} has neither generate nor file")))?;
    let text = std::fs::read_to_string(opts.base_dir.join(path))?;
    Ok(text.trim_end_matches(['\n', '\r']).to_string())
}

fn ensure_secrets(api: &mut dyn Api, file: &ComposeFile, opts: &UpOptions) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    if opts.dry_run {
        for name in file.secrets.keys() {
            let value = match api.call(Request::SecretReveal { project: file.project.clone(), name: name.clone() }) {
                Ok(Response::Text { text }) => text,
                Ok(_) | Err(Error::NotFound(_)) => "<secret>".to_string(),
                Err(e) => return Err(e),
            };
            values.insert(name.clone(), value);
        }
        return Ok(values);
    }
    let regenerate: HashSet<&String> = match &opts.regenerate_secrets {
        None => HashSet::new(),
        Some(list) if list.is_empty() => file.secrets.iter().filter(|(_, d)| d.generate.is_some()).map(|(n, _)| n).collect(),
        Some(list) => file.secrets.keys().filter(|n| list.contains(n)).collect(),
    };
    for (name, def) in &file.secrets {
        if regenerate.contains(name) {
            match api.call(Request::SecretRemove { project: file.project.clone(), name: name.clone() }) {
                Ok(_) | Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(gen) = &def.generate {
            api.call(Request::SecretEnsure {
                project: file.project.clone(),
                name: name.clone(),
                generate: gen.clone(),
                length: def.length.unwrap_or(24),
            })?;
        } else {
            let wanted = secret_value_from_file(file, name, opts)?;
            let current = match api.call(Request::SecretReveal { project: file.project.clone(), name: name.clone() }) {
                Ok(Response::Text { text }) => Some(text),
                Ok(_) | Err(Error::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if current.as_deref() != Some(wanted.as_str()) {
                api.call(Request::SecretSet { project: file.project.clone(), name: name.clone(), value: wanted })?;
            }
        }
        match api.call(Request::SecretReveal { project: file.project.clone(), name: name.clone() })? {
            Response::Text { text } => {
                values.insert(name.clone(), text);
            }
            other => return Err(Error::Internal(format!("unexpected response {other:?}"))),
        }
    }
    Ok(values)
}

fn service_references_secret(file: &ComposeFile, service: &str, secret: &str) -> bool {
    let svc = &file.services[service];
    if svc.secrets.iter().any(|s| s == secret) {
        return true;
    }
    svc.env
        .values()
        .chain(&svc.command)
        .chain(&svc.entrypoint)
        .any(|t| references(t).unwrap_or_default().iter().any(|r| matches!(r, Reference::Secret(n) if n == secret)))
}

pub struct BuildState {
    order: Vec<String>,
    ipam: Ipam,
    secrets: HashMap<String, String>,
    addresses: HashMap<String, Vec<Ipv4Addr>>,
    lb_vips: HashMap<String, Ipv4Addr>,
    forced: HashSet<String>,
    images: RefCell<HashMap<String, ImageMeta>>,
}

impl BuildState {
    pub fn new(api: &mut dyn Api, file: &ComposeFile, opts: &UpOptions) -> Result<BuildState> {
        let order = topo_order(&file.dependency_graph())?;
        let existing = containers(api, &file.project)?;
        let mut ipam = Ipam::new(Subnet::parse(&opts.subnet)?);
        for c in &existing {
            if let (Some(addr), Some(svc)) = (c.address, &c.service) {
                if let Some(idx) = replica_index(&file.project, svc, &c.name) {
                    let _ = ipam.reserve(addr, &format!("{}/{}/{}", file.project, svc, idx));
                }
            }
        }
        let secrets = ensure_secrets(api, file, opts)?;
        let mut forced = HashSet::new();
        if let Some(list) = &opts.regenerate_secrets {
            for (name, def) in &file.secrets {
                if (list.is_empty() && def.generate.is_some()) || list.contains(name) {
                    for svc in file.services.keys() {
                        if service_references_secret(file, svc, name) {
                            forced.insert(svc.clone());
                        }
                    }
                }
            }
        }
        let mut addresses: HashMap<String, Vec<Ipv4Addr>> = HashMap::new();
        for svc in &order {
            for idx in 0..file.services[svc].replicas {
                addresses.entry(svc.clone()).or_default().push(ipam.for_service(&file.project, svc, idx)?);
            }
        }
        let mut lb_vips = HashMap::new();
        for name in file.loadbalancers.keys() {
            lb_vips.insert(name.clone(), ipam.vip(&file.project, name)?);
        }
        Ok(BuildState { order, ipam, secrets, addresses, lb_vips, forced, images: RefCell::new(HashMap::new()) })
    }

    pub fn spec(&mut self, file: &ComposeFile, opts: &UpOptions, service: &str, idx: u32) -> Result<Spec> {
        let base = |_: Series| Ok("latest".to_string());
        let tctx = Context { secrets: self.secrets.clone(), addresses: self.addresses.clone(), lb_addresses: self.lb_vips.clone() };
        let registries = file.registries.clone();
        let state_dir = opts.state_dir.clone();
        let dry_run = opts.dry_run;
        let resolved = &self.images;
        let oci = |image: &str, policy: PullPolicy| -> Result<ImageMeta> {
            if let Some(meta) = resolved.borrow().get(image) {
                return Ok(meta.clone());
            }
            let reference = OciReference::parse(image).map_err(|e| Error::Invalid(format!("image {image}: {e}")))?;
            let creds = ComposeCredentials { registries: &registries, tctx: &tctx };
            let policy = if dry_run { PullPolicy::Never } else { policy };
            let meta = collocate_image::pull::ensure_pulled(&reference, &state_dir, &creds, policy)?;
            resolved.borrow_mut().insert(image.to_string(), meta.clone());
            Ok(meta)
        };
        let base_dir = opts.base_dir.clone();
        let dry = opts.dry_run;
        let cfgs = file.configs.clone();
        let config_path = |name: &str| -> Result<String> {
            let def = cfgs.get(name).ok_or_else(|| Error::NotFound(format!("config {name}")))?;
            let rendered = resolve(&std::fs::read_to_string(base_dir.join(&def.template))?, &tctx)?;
            let dir = base_dir.join(".collocate").join("rendered");
            let out = dir.join(name);
            if !dry {
                std::fs::create_dir_all(&dir)?;
                std::fs::write(&out, rendered)?;
            }
            Ok(out.to_string_lossy().into_owned())
        };
        let ctx = BuildCtx {
            secrets: &self.secrets,
            addresses: &self.addresses,
            lb_addresses: &self.lb_vips,
            base_build: &base,
            oci: &oci,
            config_path: &config_path,
        };
        let mut spec = build_spec(file, service, idx, &ctx)?;
        spec.net.addr = Some(self.ipam.for_service(&file.project, service, idx)?);
        Ok(spec)
    }

    pub fn is_forced(&self, service: &str) -> bool {
        self.forced.contains(service)
    }

    pub fn lb_vip(&self, name: &str) -> Option<Ipv4Addr> {
        self.lb_vips.get(name).copied()
    }
}

fn prepare(api: &mut dyn Api, file: &ComposeFile, opts: &UpOptions) -> Result<Prepared> {
    let mut state = BuildState::new(api, file, opts)?;
    let mut specs: BTreeMap<String, Vec<Spec>> = BTreeMap::new();
    for svc in state.order.clone() {
        for idx in 0..file.services[&svc].replicas {
            specs.entry(svc.clone()).or_default().push(state.spec(file, opts, &svc, idx)?);
        }
    }
    Ok(Prepared { order: state.order.clone(), specs, lb_vips: state.lb_vips.clone(), forced: state.forced.clone() })
}

fn make_plan(prep: &Prepared, existing: &[ContainerInfo]) -> Plan {
    let mut desired = Vec::new();
    for svc in &prep.order {
        for s in &prep.specs[svc] {
            let mut hash = s.labels.revision.clone().unwrap_or_default();
            if prep.forced.contains(svc) {
                hash.push_str("+rotate");
            }
            desired.push(Item { name: s.name.clone(), hash });
        }
    }
    let actual: Vec<Actual> = existing
        .iter()
        .map(|c| Actual { name: c.name.clone(), hash: c.revision.clone().unwrap_or_default(), running: c.state == State::Running })
        .collect();
    diff(&desired, &actual)
}

pub fn plan_only(api: &mut dyn Api, file: &ComposeFile, opts: &UpOptions) -> Result<Plan> {
    let mut dry = opts.clone();
    dry.dry_run = true;
    let prep = prepare(api, file, &dry)?;
    let existing = containers(api, &file.project)?;
    Ok(make_plan(&prep, &existing))
}

fn dependency_healthcheck_override<'a>(file: &'a ComposeFile, service: &str) -> Option<&'a [String]> {
    if file.services.get(service).is_some_and(|s| s.healthcheck.is_some()) {
        return None;
    }
    file.services
        .values()
        .find_map(|s| s.depends_on.iter().find(|d| d.service == service && !d.healthcheck.is_empty()))
        .map(|d| d.healthcheck.as_slice())
}

fn wait_ready(api: &mut dyn Api, file: &ComposeFile, service: &str, names: &[String], opts: &UpOptions) -> Result<()> {
    let deadline = Instant::now() + opts.ready_timeout;
    let probe = dependency_healthcheck_override(file, service);
    loop {
        let list = containers(api, &file.project)?;
        let mut ready = true;
        for n in names {
            let Some(c) = list.iter().find(|c| &c.name == n) else {
                ready = false;
                break;
            };
            if c.state != State::Running {
                ready = false;
                break;
            }
            let healthy = match probe {
                Some(argv) => matches!(
                    api.call(Request::ExecProbe { target: n.clone(), argv: argv.to_vec(), timeout_secs: 5 }),
                    Ok(Response::Exit { status: 0 })
                ),
                None => c.health.is_none_or(|h| h == HealthState::Healthy),
            };
            if !healthy {
                ready = false;
                break;
            }
        }
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout(format!("service {service} did not become ready")));
        }
        std::thread::sleep(opts.poll_interval);
    }
}

fn expect_ok(r: Result<Response>) -> Result<()> {
    r.map(|_| ())
}

fn register_lb(api: &mut dyn Api, file: &ComposeFile, name: &str, prep: &Prepared) -> Result<()> {
    let def = &file.loadbalancers[name];
    let publish = def.publish.iter().filter_map(|p| collocate_core::net::Publish::parse(p).ok()).map(|p| p.host).collect();
    let algorithm = match def.algorithm {
        crate::model::LbAlgorithm::RoundRobin => collocate_core::net::Algorithm::RoundRobin,
        crate::model::LbAlgorithm::Random => collocate_core::net::Algorithm::Random,
        crate::model::LbAlgorithm::SourceHash => collocate_core::net::Algorithm::SourceHash,
    };
    let on_no_backends = if def.on_no_backends.as_deref() == Some("drop") {
        collocate_core::net::NoBackends::Drop
    } else {
        collocate_core::net::NoBackends::Reject
    };
    expect_ok(api.call(Request::LbSet {
        lb: LbSpec {
            project: file.project.clone(),
            name: name.to_string(),
            proto: Proto::Tcp,
            listen: def.listen,
            publish,
            backend_service: def.backends.service.clone(),
            backend_port: def.backends.port,
            algorithm,
            on_no_backends,
            drain_secs: def.drain_secs,
            vip: Some(prep.lb_vips[name]),
        },
    }))
}

pub fn up(api: &mut dyn Api, file: &ComposeFile, opts: &UpOptions) -> Result<UpReport> {
    file.validate()?;
    let prep = prepare(api, file, opts)?;
    let existing = containers(api, &file.project)?;
    let plan = make_plan(&prep, &existing);
    let mut report = UpReport { kept: plan.keep.clone(), ..UpReport::default() };
    for (label, list) in [
        (&mut report.created, &plan.create),
        (&mut report.recreated, &plan.recreate),
        (&mut report.started, &plan.start),
        (&mut report.removed, &plan.remove),
    ] {
        label.extend(list.iter().cloned());
    }
    if opts.dry_run {
        return Ok(report);
    }

    for svc in &prep.order {
        let mut touched = Vec::new();
        for spec in &prep.specs[svc] {
            let name = &spec.name;
            if plan.recreate.contains(name) {
                expect_ok(api.call(Request::Stop { target: name.clone(), timeout_secs: None }))?;
                expect_ok(api.call(Request::Rm { target: name.clone(), force: true, keep_data: false }))?;
                expect_ok(api.call(Request::Run(Box::new(spec.clone()))))?;
                touched.push(name.clone());
            } else if plan.create.contains(name) {
                expect_ok(api.call(Request::Run(Box::new(spec.clone()))))?;
                touched.push(name.clone());
            } else if plan.start.contains(name) {
                expect_ok(api.call(Request::Start { target: name.clone() }))?;
                touched.push(name.clone());
            }
        }
        if !touched.is_empty() {
            wait_ready(api, file, svc, &touched, opts)?;
        }
        for (lb_name, lb) in &file.loadbalancers {
            if &lb.backends.service == svc {
                register_lb(api, file, lb_name, &prep)?;
            }
        }
    }

    for name in shutdown_order(&plan.remove) {
        expect_ok(api.call(Request::Stop { target: name.clone(), timeout_secs: None }))?;
        expect_ok(api.call(Request::Rm { target: name, force: true, keep_data: false }))?;
    }
    Ok(report)
}

pub fn down(api: &mut dyn Api, file: &ComposeFile, opts: &UpOptions) -> Result<Vec<String>> {
    let order = shutdown_order(&topo_order(&file.dependency_graph())?);
    let existing = containers(api, &file.project)?;
    let mut removed = Vec::new();
    for svc in &order {
        let mut mine: Vec<&ContainerInfo> = existing.iter().filter(|c| c.service.as_deref() == Some(svc.as_str())).collect();
        mine.sort_by(|a, b| a.name.cmp(&b.name));
        for c in mine {
            if !opts.dry_run {
                expect_ok(api.call(Request::Stop { target: c.name.clone(), timeout_secs: None }))?;
                expect_ok(api.call(Request::Rm { target: c.name.clone(), force: true, keep_data: false }))?;
            }
            removed.push(c.name.clone());
        }
    }
    if !opts.dry_run {
        for name in file.loadbalancers.keys() {
            match api.call(Request::LbRemove { project: file.project.clone(), name: name.clone() }) {
                Ok(_) | Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(removed)
}
