use crate::client::{enroll as enroll_remote, Endpoint, HttpsApi};
use crate::config::{client_identity, config_dir, RemoteConfig, LOCAL};
use collocate_core::client::{Api, Client};
use collocate_core::layout::Layout;
use collocate_core::net::{Proto, Publish, Volume};
use collocate_core::request::{ContainerInfo, ContainerStats, LogSource, RegistryCredential, Request, Response};
use collocate_core::size::parse_size;
use collocate_core::spec::{HostEntry, Mount, RestartPolicy, RootSource, Series, Spec};
use collocate_core::{ContainerId, Error, Result};
use collocate_image::config::{spec_from_image, ImageMeta, RunOverrides};
use collocate_image::pull::PullPolicy;
use collocate_registry::Reference;
use collocate_sys::fdpass::SendWithFds;
use collocate_trust::{Identity, Token};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

const SIGTERM: i32 = 15;

struct Backend {
    local: Option<PathBuf>,
    http: Option<HttpsApi>,
}

pub struct Collocate {
    backend: Backend,
}

impl Collocate {
    pub fn local() -> Result<Collocate> {
        let layout = Layout::detect();
        Ok(Collocate { backend: Backend { local: Some(layout.socket().to_path_buf()), http: None } })
    }

    pub fn at(socket: impl AsRef<Path>) -> Collocate {
        Collocate { backend: Backend { local: Some(socket.as_ref().to_path_buf()), http: None } }
    }

    pub fn remote(name: &str) -> Result<Collocate> {
        if name == LOCAL {
            return Collocate::local();
        }
        let dir = config_dir();
        let remote = RemoteConfig::load(&dir)?.get(name)?.clone();
        let endpoint = Endpoint { addresses: remote.addresses, fingerprint: remote.fingerprint };
        let identity = client_identity(&dir)?;
        Ok(Collocate::from_endpoint(endpoint, identity))
    }

    pub fn connect(addresses: impl Into<Vec<String>>, fingerprint: impl Into<String>) -> Result<Collocate> {
        let identity = client_identity(&config_dir())?;
        Ok(Collocate::from_endpoint(Endpoint { addresses: addresses.into(), fingerprint: fingerprint.into() }, identity))
    }

    pub fn enroll(token: &str, name: Option<&str>) -> Result<Collocate> {
        let token = Token::decode(token)?;
        let identity = client_identity(&config_dir())?;
        enroll_remote(&token, &identity, name)?;
        Ok(Collocate::from_endpoint(Endpoint { addresses: token.addresses.clone(), fingerprint: token.fingerprint.clone() }, identity))
    }

    pub fn from_endpoint(endpoint: Endpoint, identity: Identity) -> Collocate {
        Collocate { backend: Backend { local: None, http: Some(HttpsApi::new(endpoint, identity)) } }
    }

    pub fn run(&mut self, spec: Spec) -> Result<ContainerId> {
        match self.call(Request::Run(Box::new(spec)))? {
            Response::Id { id } => Ok(id),
            other => Err(unexpected("run", other)),
        }
    }

    pub fn specify(&mut self, name: impl Into<String>) -> RunBuilder<'_> {
        RunBuilder { c: self, p: PartialSpec::new(name.into()) }
    }

    pub fn start(&mut self, target: &str) -> Result<()> {
        expect_ok(self, "start", Request::Start { target: target.to_string() })
    }

    pub fn stop(&mut self, target: &str, timeout_secs: Option<u64>) -> Result<()> {
        expect_ok(self, "stop", Request::Stop { target: target.to_string(), timeout_secs })
    }

    pub fn restart(&mut self, target: &str, timeout_secs: Option<u64>) -> Result<()> {
        expect_ok(self, "restart", Request::Restart { target: target.to_string(), timeout_secs })
    }

    pub fn kill(&mut self, target: &str) -> Result<()> {
        self.kill_signal(target, SIGTERM)
    }

    pub fn kill_signal(&mut self, target: &str, signal: i32) -> Result<()> {
        expect_ok(self, "kill", Request::Kill { target: target.to_string(), signal })
    }

    pub fn rm(&mut self, target: &str) -> Result<()> {
        self.remove(target, false, false)
    }

    pub fn rm_force(&mut self, target: &str) -> Result<()> {
        self.remove(target, true, false)
    }

    pub fn remove(&mut self, target: &str, force: bool, keep_data: bool) -> Result<()> {
        expect_ok(self, "rm", Request::Rm { target: target.to_string(), force, keep_data })
    }

    pub fn wait(&mut self, target: &str) -> Result<i32> {
        match self.call(Request::Wait { target: target.to_string() })? {
            Response::Exit { status } => Ok(status),
            other => Err(unexpected("wait", other)),
        }
    }

    pub fn commit(&mut self, target: &str, image: Option<&str>) -> Result<String> {
        let image = image.unwrap_or_default().to_string();
        match self.call(Request::Commit { target: target.to_string(), image })? {
            Response::Text { text } => Ok(text),
            other => Err(unexpected("commit", other)),
        }
    }

    pub fn ps(&mut self, all: bool, project: Option<&str>) -> Result<Vec<ContainerInfo>> {
        match self.call(Request::Ps { all, project: project.map(str::to_string) })? {
            Response::Containers(list) => Ok(list),
            other => Err(unexpected("ps", other)),
        }
    }

    pub fn logs(&mut self, target: &str, opts: &Logs) -> Result<LogWindow> {
        let source = opts.source;
        match self.call(Request::Logs { target: target.to_string(), tail: opts.tail, offset: None, source, services: opts.services.clone() })? {
            Response::Log { data, next_offset, source } => Ok(LogWindow { data, next_offset, source }),
            other => Err(unexpected("logs", other)),
        }
    }

    pub fn stats(&mut self, project: Option<&str>) -> Result<Vec<ContainerStats>> {
        match self.call(Request::Stats { project: project.map(str::to_string) })? {
            Response::Stats(list) => Ok(list),
            other => Err(unexpected("stats", other)),
        }
    }

    pub fn info(&mut self) -> Result<serde_json::Value> {
        match self.call(Request::Info)? {
            Response::Text { text } => Ok(serde_json::from_str(&text)?),
            other => Err(unexpected("info", other)),
        }
    }

    pub fn exec<R, W1, W2>(
        &mut self,
        target: &str,
        argv: &[impl AsRef<str>],
        opts: &ExecOptions,
        stdin: R,
        stdout: &mut W1,
        stderr: &mut W2,
    ) -> Result<i32>
    where
        R: Read + Send + 'static,
        W1: Write + Send,
        W2: Write + Send,
    {
        let req = Request::Exec {
            target: target.to_string(),
            argv: argv.iter().map(|a| a.as_ref().to_string()).collect(),
            env: opts.env.clone(),
            user: opts.user.clone(),
            workdir: opts.workdir.clone(),
            tty: opts.tty,
            timeout_secs: opts.timeout_secs,
            service: opts.service.clone(),
        };
        match (&mut self.backend.local, &mut self.backend.http) {
            (Some(socket), _) => exec_local(socket, &req, stdin, stdout, stderr),
            (None, Some(api)) => api.exec(&req, Box::new(stdin), &mut *stdout, &mut *stderr),
            (None, None) => Err(Error::Internal("no transport configured".into())),
        }
    }

    pub fn pull(&mut self, reference: &str) -> Result<ImageMeta> {
        self.pull_with(reference, PullPolicy::Missing, Vec::new())
    }

    pub fn pull_with(&mut self, reference: &str, policy: PullPolicy, credentials: Vec<RegistryCredential>) -> Result<ImageMeta> {
        Reference::parse(reference).map_err(|e| Error::Invalid(format!("image {reference}: {e}")))?;
        match self.call(Request::ImagePull { reference: reference.to_string(), policy: policy.label().to_string(), credentials })? {
            Response::Json { value } => Ok(serde_json::from_value(value)?),
            other => Err(unexpected("pull", other)),
        }
    }

    pub fn images(&mut self) -> Result<Vec<ImageMeta>> {
        match self.call(Request::ImageList)? {
            Response::Json { value } => Ok(serde_json::from_value(value)?),
            other => Err(unexpected("images", other)),
        }
    }

    pub fn image(&mut self, reference: &str) -> Result<ImageMeta> {
        self.image_meta(reference)
    }

    pub fn image_meta(&mut self, reference: &str) -> Result<ImageMeta> {
        match self.call(Request::ImageShow { name: reference.to_string() })? {
            Response::Json { value } => Ok(serde_json::from_value(value)?),
            other => Err(unexpected("image_meta", other)),
        }
    }

    pub fn image_remove(&mut self, reference: &str) -> Result<()> {
        expect_ok(self, "image_remove", Request::ImageDelete { name: reference.to_string() })
    }

    pub fn image_prune(&mut self) -> Result<Vec<String>> {
        match self.call(Request::ImagePrune)? {
            Response::Json { value } => Ok(serde_json::from_value(value)?),
            other => Err(unexpected("image_prune", other)),
        }
    }

    pub fn shutdown(&mut self) -> Result<()> {
        expect_ok(self, "shutdown", Request::Shutdown)
    }

    fn image_meta_or_pull(&mut self, reference: &str) -> Result<ImageMeta> {
        match self.image_meta(reference) {
            Ok(meta) => Ok(meta),
            Err(Error::NotFound(_)) => {
                if Reference::parse(reference).is_err() {
                    return Err(Error::NotFound(format!("image {reference} is not present locally")));
                }
                self.pull(reference)
            }
            Err(e) => Err(e),
        }
    }
}

impl Api for Collocate {
    fn call(&mut self, req: Request) -> Result<Response> {
        match (&mut self.backend.local, &mut self.backend.http) {
            (Some(socket), _) => {
                let mut client = Client::connect(socket)?;
                client.call(&req)
            }
            (None, Some(api)) => api.call(req),
            (None, None) => Err(Error::Internal("no transport configured".into())),
        }
    }
}

fn expect_ok(c: &mut Collocate, verb: &str, req: Request) -> Result<()> {
    match c.call(req)? {
        Response::Ok => Ok(()),
        other => Err(unexpected(verb, other)),
    }
}

fn unexpected(verb: &str, resp: Response) -> Error {
    Error::Internal(format!("{verb}: unexpected response {resp:?}"))
}

fn exec_local<R, W1, W2>(
    socket: &Path,
    req: &Request,
    mut stdin: R,
    stdout: &mut W1,
    stderr: &mut W2,
) -> Result<i32>
where
    R: Read + Send + 'static,
    W1: Write + Send,
    W2: Write + Send,
{
    let (stdin_reader, mut stdin_writer) = UnixStream::pair()?;
    let (mut stdout_reader, stdout_writer) = UnixStream::pair()?;
    let (mut stderr_reader, stderr_writer) = UnixStream::pair()?;
    let mut client = Client::connect(socket)?;
    client.send_with_fds(req, &[&stdin_reader.as_raw_fd(), &stdout_writer.as_raw_fd(), &stderr_writer.as_raw_fd()])?;
    drop(stdin_reader);
    drop(stdout_writer);
    drop(stderr_writer);
    std::thread::scope(|scope| {
        scope.spawn(move || pump(&mut stdin, &mut stdin_writer));
        scope.spawn(move || pump(&mut stdout_reader, &mut *stdout));
        scope.spawn(move || pump(&mut stderr_reader, &mut *stderr));
        match client.read_response()? {
            Response::Exit { status } => Ok(status),
            other => Err(unexpected("exec", other)),
        }
    })
}

fn pump(src: &mut impl Read, dst: &mut impl Write) {
    let mut buf = vec![0u8; 16384];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub env: Vec<(String, String)>,
    pub user: Option<String>,
    pub workdir: Option<String>,
    pub tty: bool,
    pub timeout_secs: Option<u64>,
    pub service: Option<String>,
}

impl ExecOptions {
    pub fn new() -> ExecOptions {
        ExecOptions::default()
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> ExecOptions {
        set_env(&mut self.env, key.into(), value.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct Logs {
    pub tail: Option<usize>,
    pub source: LogSource,
    pub services: Vec<String>,
}

impl Logs {
    pub fn new() -> Logs {
        Logs::default()
    }

    pub fn tail(mut self, tail: usize) -> Logs {
        self.tail = Some(tail);
        self
    }
}

#[derive(Debug, Clone)]
pub struct LogWindow {
    pub data: String,
    pub next_offset: u64,
    pub source: LogSource,
}

#[derive(Debug, Clone, Default)]
struct PartialSpec {
    name: String,
    root: Option<RootSource>,
    image: Option<String>,
    entrypoint: Option<Vec<String>>,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    user: Option<String>,
    workdir: Option<String>,
    hostname: Option<String>,
    series: Option<Series>,
    build_id: String,
    persistent: bool,
    idle_timeout_secs: Option<u64>,
    read_only: bool,
    restart: Option<RestartPolicy>,
    stop_signal: Option<i32>,
    stop_timeout_secs: Option<u64>,
    cpus_milli: Option<u32>,
    cpu_weight: Option<u32>,
    memory: Option<u64>,
    memory_text: Option<String>,
    swap: Option<u64>,
    swap_text: Option<String>,
    pids: Option<u64>,
    mounts: Vec<Mount>,
    volume_text: Vec<String>,
    tmpfs_text: Vec<String>,
    publish_text: Vec<String>,
    published: Vec<Publish>,
    publish_exposed: bool,
    dns_text: Vec<String>,
    extra_hosts: Vec<HostEntry>,
    cap_adds: Vec<String>,
    cap_drops: Vec<String>,
    project: Option<String>,
    service: Option<String>,
    node: Option<String>,
    revision: Option<String>,
    replica: Option<u32>,
    extra_labels: Vec<(String, String)>,
}

impl PartialSpec {
    fn new(name: String) -> PartialSpec {
        PartialSpec { name, build_id: "latest".into(), ..PartialSpec::default() }
    }
}

pub struct RunBuilder<'a> {
    c: &'a mut Collocate,
    p: PartialSpec,
}

impl<'a> RunBuilder<'a> {
    pub fn image(&mut self, name: impl AsRef<str>) -> &mut Self {
        self.p.image = Some(name.as_ref().to_string());
        self
    }

    pub fn base(&mut self, series: impl AsRef<str>) -> &mut Self {
        self.p.root = Some(RootSource::Base { series: Series::parse(series.as_ref()).unwrap_or(Series::Noble), build_id: self.p.build_id.clone() });
        self
    }

    pub fn build_id(&mut self, build_id: impl Into<String>) -> &mut Self {
        self.p.build_id = build_id.into();
        if let Some(RootSource::Base { build_id, .. }) = &mut self.p.root {
            *build_id = self.p.build_id.clone();
        }
        self
    }

    pub fn series(&mut self, name: impl AsRef<str>) -> &mut Self {
        if let Ok(series) = Series::parse(name.as_ref()) {
            self.p.series = Some(series);
        }
        self
    }

    pub fn command(&mut self, argv: &[impl AsRef<str>]) -> &mut Self {
        self.p.argv = argv.iter().map(|a| a.as_ref().to_string()).collect();
        self
    }

    pub fn entrypoint(&mut self, argv: &[impl AsRef<str>]) -> &mut Self {
        self.p.entrypoint = Some(argv.iter().map(|a| a.as_ref().to_string()).collect());
        self
    }

    pub fn env(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        set_env(&mut self.p.env, key.into(), value.into());
        self
    }

    pub fn envs(&mut self, env: &[(impl AsRef<str>, impl AsRef<str>)]) -> &mut Self {
        for (k, v) in env {
            set_env(&mut self.p.env, String::from(k.as_ref()), String::from(v.as_ref()));
        }
        self
    }

    pub fn user(&mut self, user: impl Into<String>) -> &mut Self {
        self.p.user = Some(user.into());
        self
    }

    pub fn workdir(&mut self, workdir: impl Into<String>) -> &mut Self {
        self.p.workdir = Some(workdir.into());
        self
    }

    pub fn hostname(&mut self, hostname: impl Into<String>) -> &mut Self {
        self.p.hostname = Some(hostname.into());
        self
    }

    pub fn persistent(&mut self) -> &mut Self {
        self.p.persistent = true;
        self
    }

    pub fn idle_timeout(&mut self, secs: u64) -> &mut Self {
        self.p.idle_timeout_secs = Some(secs);
        self
    }

    pub fn read_only(&mut self) -> &mut Self {
        self.p.read_only = true;
        self
    }

    pub fn restart_always(&mut self) -> &mut Self {
        self.p.restart = Some(RestartPolicy::Always);
        self
    }

    pub fn restart_on_failure(&mut self, max: u32) -> &mut Self {
        self.p.restart = Some(RestartPolicy::OnFailure { max });
        self
    }

    pub fn stop_signal(&mut self, signal: i32) -> &mut Self {
        self.p.stop_signal = Some(signal);
        self
    }

    pub fn stop_timeout(&mut self, secs: u64) -> &mut Self {
        self.p.stop_timeout_secs = Some(secs);
        self
    }

    pub fn cpu(&mut self, cores: f64) -> &mut Self {
        self.p.cpus_milli = Some((cores * 1000.0).round() as u32);
        self
    }

    pub fn cpu_milli(&mut self, milli: u32) -> &mut Self {
        self.p.cpus_milli = Some(milli);
        self
    }

    pub fn cpu_weight(&mut self, weight: u32) -> &mut Self {
        self.p.cpu_weight = Some(weight);
        self
    }

    pub fn ram(&mut self, text: impl AsRef<str>) -> &mut Self {
        self.p.memory_text = Some(text.as_ref().to_string());
        self.p.memory = None;
        self
    }

    pub fn ram_bytes(&mut self, bytes: u64) -> &mut Self {
        self.p.memory = Some(bytes);
        self.p.memory_text = None;
        self
    }

    pub fn swap(&mut self, text: impl AsRef<str>) -> &mut Self {
        self.p.swap_text = Some(text.as_ref().to_string());
        self.p.swap = None;
        self
    }

    pub fn swap_bytes(&mut self, bytes: u64) -> &mut Self {
        self.p.swap = Some(bytes);
        self.p.swap_text = None;
        self
    }

    pub fn pids(&mut self, max: u64) -> &mut Self {
        self.p.pids = Some(max);
        self
    }

    pub fn volume(&mut self, text: impl AsRef<str>) -> &mut Self {
        self.p.volume_text.push(text.as_ref().to_string());
        self
    }

    pub fn bind(&mut self, src: impl Into<String>, dst: impl Into<String>, ro: bool) -> &mut Self {
        self.p.mounts.push(Mount::Bind { src: src.into(), dst: dst.into(), ro });
        self
    }

    pub fn tmpfs(&mut self, text: impl AsRef<str>) -> &mut Self {
        self.p.tmpfs_text.push(text.as_ref().to_string());
        self
    }

    pub fn secret(&mut self, name: impl Into<String>, dst: Option<&str>) -> &mut Self {
        let name = name.into();
        self.p.mounts.push(Mount::Secret { dst: dst.map_or_else(|| format!("/run/secrets/{name}"), String::from), name });
        self
    }

    pub fn mount(&mut self, mount: Mount) -> &mut Self {
        self.p.mounts.push(mount);
        self
    }

    pub fn publish(&mut self, text: impl AsRef<str>) -> &mut Self {
        self.p.publish_text.push(text.as_ref().to_string());
        self
    }

    pub fn publish_tcp(&mut self, host: u16, container: u16) -> &mut Self {
        self.p.published.push(Publish { host, container, proto: Proto::Tcp });
        self
    }

    pub fn publish_udp(&mut self, host: u16, container: u16) -> &mut Self {
        self.p.published.push(Publish { host, container, proto: Proto::Udp });
        self
    }

    pub fn publish_exposed(&mut self) -> &mut Self {
        self.p.publish_exposed = true;
        self
    }

    pub fn dns(&mut self, addr: impl AsRef<str>) -> &mut Self {
        self.p.dns_text.push(addr.as_ref().to_string());
        self
    }

    pub fn extra_host(&mut self, name: impl Into<String>, addr: IpAddr) -> &mut Self {
        self.p.extra_hosts.push(HostEntry { name: name.into(), addr });
        self
    }

    pub fn cap_add(&mut self, cap: impl AsRef<str>) -> &mut Self {
        self.p.cap_adds.push(cap.as_ref().to_string());
        self
    }

    pub fn cap_adds(&mut self, caps: &[impl AsRef<str>]) -> &mut Self {
        self.p.cap_adds.extend(caps.iter().map(|c| c.as_ref().to_string()));
        self
    }

    pub fn cap_drop(&mut self, cap: impl AsRef<str>) -> &mut Self {
        self.p.cap_drops.push(cap.as_ref().to_string());
        self
    }

    pub fn cap_drops(&mut self, caps: &[impl AsRef<str>]) -> &mut Self {
        self.p.cap_drops.extend(caps.iter().map(|c| c.as_ref().to_string()));
        self
    }

    pub fn project(&mut self, project: impl Into<String>) -> &mut Self {
        self.p.project = Some(project.into());
        self
    }

    pub fn service(&mut self, service: impl Into<String>) -> &mut Self {
        self.p.service = Some(service.into());
        self
    }

    pub fn node(&mut self, node: impl Into<String>) -> &mut Self {
        self.p.node = Some(node.into());
        self
    }

    pub fn revision(&mut self, revision: impl Into<String>) -> &mut Self {
        self.p.revision = Some(revision.into());
        self
    }

    pub fn replica(&mut self, replica: u32) -> &mut Self {
        self.p.replica = Some(replica);
        self
    }

    pub fn label(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.p.extra_labels.push((key.into(), value.into()));
        self
    }

    pub fn build(&mut self) -> Result<Spec> {
        let name = self.p.name.clone();
        let mut spec = match (&self.p.root, &self.p.image) {
            (Some(root), _) => {
                let argv = merge_argv(&self.p);
                if argv.is_empty() {
                    return Err(no_command(&name));
                }
                Spec::new(&name, root.clone(), argv)
            }
            (None, Some(reference)) => {
                let override_argv = self.p.argv.clone();
                let override_entrypoint = self.p.entrypoint.clone();
                let publish_exposed = self.p.publish_exposed;
                let meta = self.c.image_meta_or_pull(reference)?;
                let ov = RunOverrides {
                    command: override_argv,
                    entrypoint: override_entrypoint,
                    env: Vec::new(),
                    user: None,
                    workdir: None,
                    publish_exposed,
                };
                spec_from_image(&meta, &ov)?
            }
            (None, None) => {
                let argv = merge_argv(&self.p);
                if argv.is_empty() {
                    return Err(no_command(&name));
                }
                Spec::new(&name, RootSource::Base { series: self.p.series.unwrap_or(Series::Noble), build_id: self.p.build_id.clone() }, argv)
            }
        };
        apply(&mut spec, &self.p)?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn run(&mut self) -> Result<ContainerId> {
        let spec = self.build()?;
        self.c.run(spec)
    }
}

fn merge_argv(p: &PartialSpec) -> Vec<String> {
    let mut argv = p.entrypoint.clone().unwrap_or_default();
    argv.extend(p.argv.clone());
    argv
}

fn no_command(name: &str) -> Error {
    Error::InvalidSpec(format!("container {name:?} needs a command or an image"))
}

fn set_env(env: &mut Vec<(String, String)>, key: String, value: String) {
    match env.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => env.push((key, value)),
    }
}

fn apply(spec: &mut Spec, p: &PartialSpec) -> Result<()> {
    if spec.name.is_empty() {
        spec.name = p.name.clone();
    }
    if let Some(hostname) = &p.hostname {
        spec.hostname = hostname.clone();
    } else if spec.hostname.is_empty() {
        spec.hostname = spec.name.clone();
    }
    spec.persistent = p.persistent;
    if let Some(t) = p.idle_timeout_secs {
        spec.idle_timeout_secs = Some(t);
    }
    spec.read_only_rootfs = p.read_only;
    if let Some(restart) = p.restart {
        spec.restart = restart;
    }
    for (k, v) in &p.env {
        set_env(&mut spec.process.env, k.clone(), v.clone());
    }
    if let Some(user) = &p.user {
        spec.process.user = user.clone();
    }
    if let Some(workdir) = &p.workdir {
        spec.process.workdir = workdir.clone();
    }
    if let Some(signal) = p.stop_signal {
        spec.process.stop_signal = signal;
    }
    if let Some(secs) = p.stop_timeout_secs {
        spec.process.stop_timeout_secs = secs;
    }
    if let Some(milli) = p.cpus_milli {
        spec.limits.cpus_milli = Some(milli);
    }
    if let Some(weight) = p.cpu_weight {
        spec.limits.cpu_weight = Some(weight);
    }
    if let Some(bytes) = p.memory {
        spec.limits.memory = Some(bytes);
    } else if let Some(text) = &p.memory_text {
        spec.limits.memory = Some(parse_size(text)?);
    }
    if let Some(bytes) = p.swap {
        spec.limits.swap = Some(bytes);
    } else if let Some(text) = &p.swap_text {
        spec.limits.swap = Some(parse_size(text)?);
    }
    if let Some(max) = p.pids {
        spec.limits.pids_max = max;
    }
    spec.mounts.extend(p.mounts.clone());
    for text in &p.volume_text {
        let volume = Volume::parse(text)?;
        spec.mounts.push(if volume.is_named() {
            Mount::Volume { name: volume.src, dst: volume.dst }
        } else {
            Mount::Bind { src: volume.src, dst: volume.dst, ro: volume.ro }
        });
    }
    for text in &p.tmpfs_text {
        spec.mounts.push(parse_tmpfs(text)?);
    }
    for text in &p.publish_text {
        spec.net.publish.push(Publish::parse(text)?);
    }
    spec.net.publish.extend(p.published.clone());
    spec.dns.extend(p.dns_text.iter().map(|d| d.parse()).collect::<std::result::Result<Vec<IpAddr>, _>>().map_err(|_| Error::Invalid("invalid dns address".into()))?);
    spec.extra_hosts.extend(p.extra_hosts.clone());
    spec.caps.add.extend(p.cap_adds.clone());
    spec.caps.drop.extend(p.cap_drops.clone());
    if let Some(project) = &p.project {
        spec.labels.project = Some(project.clone());
    }
    if let Some(service) = &p.service {
        spec.labels.service = Some(service.clone());
    }
    if let Some(node) = &p.node {
        spec.labels.node = Some(node.clone());
    }
    if let Some(revision) = &p.revision {
        spec.labels.revision = Some(revision.clone());
    }
    if let Some(replica) = p.replica {
        spec.labels.replica = Some(replica);
    }
    for (k, v) in &p.extra_labels {
        spec.labels.extra.insert(k.clone(), v.clone());
    }
    Ok(())
}

fn parse_tmpfs(spec: &str) -> Result<Mount> {
    let (dst, opts) = spec.split_once(':').unwrap_or((spec, ""));
    let mut size = None;
    for opt in opts.split(',').filter(|o| !o.is_empty()) {
        if let Some(v) = opt.strip_prefix("size=") {
            size = Some(parse_size(v)?);
        }
    }
    Ok(Mount::Tmpfs { dst: dst.to_string(), size })
}