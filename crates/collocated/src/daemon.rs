use crate::config::{Config, RootModeSetting};
use crate::health::{http_check, tcp_check, HealthTracker};
use crate::restart::{restart_delay, should_restart};
use crate::secrets::SecretStore;
use collocate_core::id::resolve_ref;
use collocate_core::request::{ContainerInfo, ContainerStats, HealthState, LbSpec, LbStatus, LogSource, Request, Response, State};
use collocate_core::spec::{HealthKind, ImageKind, Mount, RootSource, Spec};
use collocate_core::wire::MAX_FRAME;
use collocate_core::{ContainerId, Error, Result};
use collocate_net::files::{parse_nameservers, render_hosts, render_resolv};
use collocate_net::ipam::{Ipam, Subnet};
use collocate_net::netcfg::{bridge_setup, teardown_veth, Cmd};
use collocate_net::ruleset::{render, Backend, ContainerPorts, LbRule, Ruleset};
use collocate_runtime::cgroup::CgroupTree;
use collocate_runtime::exec::{spawn_exec, ExecRequest};
use collocate_runtime::launch::{spawn, LaunchRequest, NetSetup, RootMode};
use collocate_runtime::mountplan::Extras;
use collocate_runtime::user::resolve_user;
pub use collocate_store::instance_id;
use collocate_store::reconcile::{plan, Action, Observed};
use collocate_store::{NodeMeta, NodeStore, Runtime, StoreLock};
use collocate_sys::epoll::{Epoll, Events};
use collocate_sys::fdpass::recv_fd;
use collocate_sys::pidfd::{pidfd_open, send_signal, wait, ExitStatus};
use collocate_sys::signals::SignalFd;
use collocate_sys::timer::Timer;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LISTENER: u64 = 1;
const TIMER: u64 = 2;
const SIGNALS: u64 = 3;
const FIRST_DYNAMIC: u64 = 100;
const EXITED_GRACE: Duration = Duration::from_secs(30);
const HEALTH_TICK: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);

macro_rules! log {
    ($($arg:tt)*) => { eprintln!("collocated: {}", format!($($arg)*)) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitKind {
    Wait,
    Stop,
}

struct Waiter {
    conn: u64,
    kind: WaitKind,
}

struct Live {
    spec: Spec,
    pid: i32,
    pidfd: OwnedFd,
    token: u64,
    child: bool,
    started: Instant,
    waiters: Vec<Waiter>,
    stop_requested: bool,
    signal_at: Option<Instant>,
    kill_at: Option<Instant>,
    tracker: Option<HealthTracker>,
    last_check: Instant,
    last_activity: Instant,
}

struct Conn {
    stream: UnixStream,
    rbuf: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

struct ExecProc {
    pidfd: OwnedFd,
    conn: u64,
    deadline: Option<Instant>,
}

struct ExecArgs {
    target: String,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    user: Option<String>,
    workdir: Option<String>,
    timeout_secs: Option<u64>,
}

#[derive(Clone, Copy)]
enum ProbeOrigin {
    Health(ContainerId),
    Client(u64),
}

struct Probe {
    pidfd: OwnedFd,
    origin: ProbeOrigin,
    deadline: Instant,
}

enum Task {
    Relaunch(ContainerId),
    RemoveExited(ContainerId),
    ReapIdle(ContainerId),
}

pub struct Daemon {
    cfg: Config,
    store: NodeStore,
    _lock: StoreLock,
    node: NodeMeta,
    cgroups: CgroupTree,
    ipam: Ipam,
    secrets: SecretStore,
    root_mode: RootMode,
    epoll: Epoll,
    listener: UnixListener,
    timer: Timer,
    signals: SignalFd,
    conns: HashMap<u64, Conn>,
    live: HashMap<ContainerId, Live>,
    pid_tokens: HashMap<u64, ContainerId>,
    execs: HashMap<u64, ExecProc>,
    probes: HashMap<u64, Probe>,
    fuse_procs: HashMap<ContainerId, std::process::Child>,
    lbs: BTreeMap<String, LbSpec>,
    schedule: Vec<(Instant, Task)>,
    restarts: HashMap<ContainerId, u32>,
    user_stopped: HashSet<ContainerId>,
    next_token: u64,
    shutdown: bool,
    fw_dirty: bool,
    last_health: Instant,
}

fn io<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(Error::from)
}

fn group_gid(name: &str) -> Option<u32> {
    fs::read_to_string("/etc/group").ok()?.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.first() == Some(&name)).then(|| f.get(2)?.parse().ok()).flatten()
    })
}

fn starttime(pid: i32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    collocate_core::procinfo::parse_stat(&text).ok().map(|s| s.starttime)
}

fn lb_key(project: &str, name: &str) -> String {
    format!("{project}__{name}")
}

impl Daemon {
    pub fn new(cfg: Config) -> Result<Daemon> {
        for d in [&cfg.state_dir, &cfg.run_dir] {
            io(fs::create_dir_all(d))?;
        }
        let store = NodeStore::open(&cfg.state_dir, &cfg.run_dir)?;
        let lock = store.lock()?;
        store.recover()?;
        let node_name = cfg.node_name.clone().unwrap_or_else(|| "local".to_string());
        let node = store.open_node(&node_name, &cfg.subnet, &instance_id())?;
        let subnet = Subnet::parse(&cfg.subnet)?;

        let cgroups = CgroupTree::new(&cfg.cgroup_root, &cfg.cgroup_slice);
        io(cgroups.setup(cfg.move_self_to_supervisor))?;

        let specs = store.load_all()?;
        let entries = specs.iter().filter_map(|s| s.net.addr.map(|a| (s.id.to_string(), a)));
        let ipam = Ipam::rebuild(subnet.clone(), entries)?;

        let root_mode = match cfg.root_mode {
            RootModeSetting::Overlay => RootMode::Overlay,
            RootModeSetting::FuseOverlay => RootMode::FuseOverlay,
            RootModeSetting::BindRo => RootMode::BindRo,
            RootModeSetting::Auto => {
                let report = collocate_sys::probe::probe();
                if report.check("overlayfs").is_some_and(|c| c.ok) {
                    RootMode::Overlay
                } else if report.check("fuse-overlayfs").is_some_and(|c| c.ok) {
                    log!("overlayfs is unavailable, using fuse-overlayfs for multi-layer images");
                    RootMode::FuseOverlay
                } else {
                    log!("overlayfs and fuse-overlayfs are both unavailable, falling back to read-only bind roots");
                    RootMode::BindRo
                }
            }
        };

        for c in bridge_setup(&cfg.bridge, &subnet) {
            c.run_allowing(Some("File exists"))?;
        }

        let epoll = Epoll::new()?;
        let _ = fs::remove_file(cfg.socket());
        let listener = io(UnixListener::bind(cfg.socket()))?;
        io(listener.set_nonblocking(true))?;
        io(fs::set_permissions(cfg.socket(), fs::Permissions::from_mode(0o660)))?;
        if let Some(gid) = group_gid(&cfg.group) {
            let _ = collocate_sys::misc::chown(cfg.socket().to_str().unwrap_or(""), 0, gid);
        }
        io(epoll.add(listener.as_raw_fd(), LISTENER, Events::READABLE))?;
        let timer = io(Timer::new())?;
        io(epoll.add(timer.as_raw_fd(), TIMER, Events::READABLE))?;
        let signals = io(SignalFd::new(&[libc::SIGTERM, libc::SIGINT]))?;
        io(epoll.add(signals.as_raw_fd(), SIGNALS, Events::READABLE))?;

        let secrets = SecretStore::new(cfg.state_dir.join("secrets"));
        let mut d = Daemon {
            cfg,
            store,
            _lock: lock,
            node,
            cgroups,
            ipam,
            secrets,
            root_mode,
            epoll,
            listener,
            timer,
            signals,
            conns: HashMap::new(),
            live: HashMap::new(),
            pid_tokens: HashMap::new(),
            execs: HashMap::new(),
            probes: HashMap::new(),
            fuse_procs: HashMap::new(),
            lbs: BTreeMap::new(),
            schedule: Vec::new(),
            restarts: HashMap::new(),
            user_stopped: HashSet::new(),
            next_token: FIRST_DYNAMIC,
            shutdown: false,
            fw_dirty: true,
            last_health: Instant::now(),
        };
        d.load_lbs()?;
        d.reconcile()?;
        d.flush_firewall();
        Ok(d)
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    fn lb_dir(&self) -> PathBuf {
        self.cfg.state_dir.join("loadbalancers")
    }

    fn load_lbs(&mut self) -> Result<()> {
        if let Ok(rd) = fs::read_dir(self.lb_dir()) {
            for e in rd.flatten() {
                if let Ok(bytes) = fs::read(e.path()) {
                    if let Ok(lb) = serde_json::from_slice::<LbSpec>(&bytes) {
                        if let Some(vip) = lb.vip {
                            let _ = self.ipam.reserve(vip, &format!("vip:{}/{}", lb.project, lb.name));
                            let _ = self.vip_cmd("add", vip).run_allowing(Some("File exists"));
                        }
                        self.lbs.insert(lb_key(&lb.project, &lb.name), lb);
                    }
                }
            }
        }
        Ok(())
    }

    fn vip_cmd(&self, op: &str, vip: std::net::Ipv4Addr) -> Cmd {
        Cmd(vec!["ip".into(), "addr".into(), op.into(), format!("{vip}/32"), "dev".into(), self.cfg.bridge.clone()])
    }

    fn reconcile(&mut self) -> Result<()> {
        let specs = self.store.load_all()?;
        let mut observed = Observed::default();
        for id in io(self.cgroups.list())? {
            if let Some(p) = self.cgroups.populated(&id) {
                observed.cgroups.insert(id, p);
            }
        }
        if let Ok(out) = Command::new("ip").args(["-o", "link"]).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(name) = line.split(':').nth(1) {
                    let name = name.trim().split('@').next().unwrap_or("").to_string();
                    if name.starts_with("vh") {
                        observed.veths.push(name);
                    }
                }
            }
        }
        for sub in ["roots", "hosts", "secrets", "eph"] {
            if let Ok(rd) = fs::read_dir(self.cfg.run_dir.join(sub)) {
                for e in rd.flatten() {
                    if let Some(id) = e.file_name().to_str().and_then(|n| ContainerId::parse(n).ok()) {
                        observed.leftovers.push(id);
                    }
                }
            }
        }
        let by_id: HashMap<ContainerId, Spec> = specs.iter().map(|s| (s.id, s.clone())).collect();
        for action in plan(&specs, &observed) {
            match action {
                Action::Adopt(id) => {
                    if let Some(spec) = by_id.get(&id) {
                        if let Err(e) = self.adopt(spec.clone()) {
                            log!("cannot adopt {id}: {e}");
                        }
                    }
                }
                Action::MarkStopped(_) => {}
                Action::RemoveSpec(id) => {
                    let _ = self.store.remove(&id);
                    self.ipam.release(&id.to_string());
                }
                Action::CleanRuntime(id) => self.remove_runtime_dirs(&id),
                Action::KillCgroup(id) => {
                    let _ = self.cgroups.kill(&id);
                }
                Action::RemoveCgroup(id) => {
                    let _ = self.cgroups.remove(&id);
                }
                Action::DeleteVeth(name) => {
                    let _ = Cmd(vec!["ip".into(), "link".into(), "del".into(), name]).run();
                }
                Action::DeleteNftTag(_) => {}
            }
        }
        Ok(())
    }

    fn adopt(&mut self, spec: Spec) -> Result<()> {
        let id = spec.id;
        let recorded = self.store.read_runtime(&id)?;
        let procs = io(self.cgroups.procs(&id))?;
        let pid = match recorded {
            Some(rt) if procs.contains(&rt.pid) && starttime(rt.pid as i32) == Some(rt.starttime) => rt.pid as i32,
            _ => *procs.iter().min().ok_or_else(|| Error::NotFound(format!("no process in cgroup of {id}")))? as i32,
        };
        let pidfd = io(pidfd_open(pid))?;
        let token = self.token();
        io(self.epoll.add(pidfd.as_raw_fd(), token, Events::READABLE))?;
        self.pid_tokens.insert(token, id);
        let tracker = spec.healthcheck.as_ref().map(|h| HealthTracker::new(h.retries));
        let idle_timeout = spec.idle_timeout_secs;
        self.live.insert(
            id,
            Live {
                spec,
                pid,
                pidfd,
                token,
                child: false,
                started: Instant::now(),
                waiters: Vec::new(),
                stop_requested: false,
                signal_at: None,
                kill_at: None,
                tracker,
                last_check: Instant::now(),
                last_activity: Instant::now(),
            },
        );
        self.schedule_idle_reap(id, idle_timeout);
        log!("re-adopted container {id} (pid {pid})");
        Ok(())
    }

    fn schedule_idle_reap(&mut self, id: ContainerId, idle_timeout_secs: Option<u64>) {
        if let Some(t) = idle_timeout_secs {
            self.schedule.push((Instant::now() + Duration::from_secs(t.max(1)), Task::ReapIdle(id)));
        }
    }

    fn remove_runtime_dirs(&mut self, id: &ContainerId) {
        if let Some(mut child) = self.fuse_procs.remove(id) {
            let merged = self.cfg.run_dir.join("fusemerge").join(id.to_string());
            if let Some(m) = merged.to_str() {
                let _ = collocate_sys::mount::umount_lazy(m);
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_dir_all(&merged);
        }
        for sub in ["roots", "hosts", "secrets", "eph"] {
            let _ = fs::remove_dir_all(self.cfg.run_dir.join(sub).join(id.to_string()));
        }
        let _ = self.store.clear_runtime(id);
    }

    fn resolve(&self, target: &str) -> Result<ContainerId> {
        let all: Vec<(ContainerId, String)> = self.store.load_all()?.into_iter().map(|s| (s.id, s.name)).collect();
        resolve_ref(target, &all)
    }

    fn log_path(&self, spec: &Spec) -> PathBuf {
        if spec.persistent {
            self.store.persistent_dir(&spec.id).join("log")
        } else {
            self.store.runtime_dir(&spec.id).join("log")
        }
    }

    fn lowers(&self, root: &RootSource) -> Result<Vec<PathBuf>> {
        match root {
            RootSource::Base { series, build_id } => {
                let base = self.cfg.images_dir().join(series.dir_name());
                let build = if build_id == "latest" {
                    let link = io(fs::read_link(base.join("latest"))
                        .map_err(|e| std::io::Error::new(e.kind(), format!("no image for {}: {e}", series.dir_name()))))?;
                    link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    build_id.clone()
                };
                let rootfs = base.join(&build).join("rootfs");
                if !rootfs.is_dir() {
                    return Err(Error::NotFound(format!("image {}/{build}", series.dir_name())));
                }
                Ok(vec![rootfs])
            }
            RootSource::Oci { layers, .. } => {
                if self.root_mode == RootMode::BindRo && layers.len() > 1 {
                    return Err(Error::Invalid("multi-layer images need overlayfs, which this host does not provide".into()));
                }
                let mut out = Vec::new();
                for l in layers.iter().rev() {
                    let dir = self.cfg.state_dir.join("layers").join(l.replace(':', "-"));
                    if !dir.is_dir() {
                        return Err(Error::NotFound(format!("layer {l}")));
                    }
                    out.push(dir);
                }
                Ok(out)
            }
        }
    }

    fn prepare_fuse_overlay(&mut self, id: &ContainerId, layers: &[PathBuf], upper: &Path, work: &Path) -> Result<PathBuf> {
        let merged = self.cfg.run_dir.join("fusemerge").join(id.to_string());
        io(fs::create_dir_all(&merged))?;
        let merged_str = merged.to_str().ok_or_else(|| Error::Invalid("non-utf8 fuse merge path".into()))?.to_string();
        let lowerdir = layers.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(":");
        let opt = format!("lowerdir={lowerdir},upperdir={},workdir={},auto_unmount", upper.display(), work.display());
        let mut child = Command::new("fuse-overlayfs")
            .args(["-f", "-o", &opt, &merged_str])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Internal(format!("fuse-overlayfs: {e}")))?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let mut err_out = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut err_out);
                }
                return Err(Error::Internal(format!("fuse-overlayfs exited early ({status}): {}", err_out.trim())));
            }
            let mounted = fs::read_to_string("/proc/self/mountinfo")
                .map(|t| collocate_sys::mount::is_mount_point(&collocate_sys::mount::parse_mountinfo(&t), &merged_str))
                .unwrap_or(false);
            if mounted {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                return Err(Error::Timeout(format!("fuse-overlayfs did not mount {merged_str} in time")));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.fuse_procs.insert(*id, child);
        Ok(merged)
    }

    fn nameservers(&self, spec: &Spec) -> Vec<std::net::IpAddr> {
        if !spec.dns.is_empty() {
            return spec.dns.clone();
        }
        for f in &self.cfg.nameserver_files {
            if let Ok(t) = fs::read_to_string(f) {
                let ns = parse_nameservers(&t);
                if !ns.is_empty() {
                    return ns;
                }
            }
        }
        Vec::new()
    }

    fn write_file(path: &std::path::Path, content: &[u8], mode: u32) -> Result<()> {
        let mut f = io(fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(path))?;
        io(f.write_all(content))
    }

    fn upper_and_work_dirs(&self, id: &ContainerId, spec: &Spec) -> (PathBuf, PathBuf) {
        if spec.persistent {
            let cdir = self.store.persistent_dir(id);
            (cdir.join("upper"), cdir.join("work"))
        } else {
            let e = self.cfg.run_dir.join("eph").join(id.to_string());
            (e.join("upper"), e.join("work"))
        }
    }

    fn launch(&mut self, mut spec: Spec) -> Result<()> {
        let id = spec.id;
        let mut lowers = self.lowers(&spec.root)?;
        let cdir = if spec.persistent { self.store.persistent_dir(&id) } else { self.store.runtime_dir(&id) };
        io(fs::create_dir_all(&cdir))?;
        let (upper, work) = self.upper_and_work_dirs(&id, &spec);
        let staging = self.cfg.run_dir.join("roots").join(id.to_string());
        let files = self.cfg.run_dir.join("hosts").join(id.to_string());
        let secrets_dir = self.cfg.run_dir.join("secrets").join(id.to_string());
        for d in [&upper, &work, &staging, &files, &secrets_dir] {
            io(fs::create_dir_all(d))?;
        }
        if self.root_mode == RootMode::FuseOverlay {
            lowers = vec![self.prepare_fuse_overlay(&id, &lowers, &upper, &work)?];
        }

        let addr = match spec.net.addr {
            Some(a) => {
                self.ipam.reserve(a, &id.to_string())?;
                a
            }
            None => self.ipam.allocate(&id.to_string())?,
        };
        if spec.net.addr != Some(addr) {
            spec.net.addr = Some(addr);
            self.store.update(&spec)?;
        }

        let peers: Vec<(String, std::net::Ipv4Addr)> = self
            .live
            .values()
            .filter(|l| l.spec.labels.project.is_some() && l.spec.labels.project == spec.labels.project)
            .filter_map(|l| Some((l.spec.labels.service.clone().unwrap_or_else(|| l.spec.hostname.clone()), l.spec.net.addr?)))
            .collect();
        let extra: Vec<(String, std::net::IpAddr)> = spec.extra_hosts.iter().map(|h| (h.name.clone(), h.addr)).collect();
        Self::write_file(&files.join("resolv.conf"), render_resolv(&self.nameservers(&spec), &[]).as_bytes(), 0o644)?;
        Self::write_file(&files.join("hosts"), render_hosts(&spec.hostname, addr, &peers, &extra).as_bytes(), 0o644)?;
        Self::write_file(&files.join("hostname"), format!("{}\n", spec.hostname).as_bytes(), 0o644)?;

        let passwd = lowers.first().and_then(|l| fs::read_to_string(l.join("etc/passwd")).ok()).unwrap_or_default();
        let group = lowers.first().and_then(|l| fs::read_to_string(l.join("etc/group")).ok()).unwrap_or_default();
        let ids = resolve_user(&spec.process.user, &passwd, &group).ok();
        let project = spec.labels.project.clone().unwrap_or_else(|| "default".to_string());
        let mut volumes = HashMap::new();
        for m in &spec.mounts {
            match m {
                Mount::Secret { name, .. } => {
                    let value = self.secrets.reveal(&project, name)?;
                    let path = secrets_dir.join(name);
                    Self::write_file(&path, value.as_bytes(), 0o400)?;
                    if let Some(i) = &ids {
                        let _ = collocate_sys::misc::chown(path.to_str().unwrap_or(""), i.uid, i.gid);
                    }
                }
                Mount::Volume { name, .. } => {
                    let dir = self.cfg.state_dir.join("volumes").join(name);
                    io(fs::create_dir_all(&dir))?;
                    volumes.insert(name.clone(), dir);
                }
                _ => {}
            }
        }

        let cgroup_dir = io(self.cgroups.create(&id, &spec.limits))?;
        let subnet = Subnet::parse(&self.cfg.subnet)?;
        let req = LaunchRequest {
            spec: &spec,
            root_mode: self.root_mode,
            lowers,
            upper,
            work,
            staging,
            extras: Extras {
                resolv: files.join("resolv.conf"),
                hosts: files.join("hosts"),
                hostname: files.join("hostname"),
                init: self.cfg.init_path.clone(),
                secrets_dir,
                volumes,
            },
            cgroup_dir,
            log_path: self.log_path(&spec),
            net: Some(NetSetup { bridge: self.cfg.bridge.clone(), addr, prefix: subnet.prefix(), gateway: subnet.gateway() }),
        };
        let started = match spawn(&req).and_then(|s| s.confirm(Duration::from_secs(10)).map(|_| s)) {
            Ok(s) => s,
            Err(e) => {
                self.cleanup_runtime(&id);
                return Err(e);
            }
        };
        let token = self.token();
        io(self.epoll.add(started.pidfd.as_raw_fd(), token, Events::READABLE))?;
        self.pid_tokens.insert(token, id);
        let tracker = spec.healthcheck.as_ref().map(|h| HealthTracker::new(h.retries));
        let idle_timeout = spec.idle_timeout_secs;
        let rt = Runtime {
            pid: started.pid as u32,
            starttime: starttime(started.pid).unwrap_or(0),
            cgroup: self.cgroups.container_dir(&id).to_string_lossy().into_owned(),
            address: Some(addr),
            health: tracker.as_ref().map(|t| t.state()),
        };
        let _ = self.store.write_runtime(&id, &rt);
        spec.exit_status = None;
        let _ = self.store.update(&spec);
        self.live.insert(
            id,
            Live {
                spec,
                pid: started.pid,
                pidfd: started.pidfd,
                token,
                child: true,
                started: Instant::now(),
                waiters: Vec::new(),
                stop_requested: false,
                signal_at: None,
                kill_at: None,
                tracker,
                last_check: Instant::now(),
                last_activity: Instant::now(),
            },
        );
        self.schedule_idle_reap(id, idle_timeout);
        self.fw_dirty = true;
        Ok(())
    }

    fn cleanup_runtime(&mut self, id: &ContainerId) {
        let _ = self.cgroups.kill(id);
        let _ = self.cgroups.remove(id);
        let _ = teardown_veth(id).run();
        self.remove_runtime_dirs(id);
    }

    fn on_exit(&mut self, id: ContainerId, code: Option<i32>) {
        let Some(mut live) = self.live.remove(&id) else { return };
        self.pid_tokens.remove(&live.token);
        let _ = self.epoll.remove(live.pidfd.as_raw_fd());
        let code = code.unwrap_or(-1);
        live.spec.exit_status = Some(code);
        let _ = self.store.update(&live.spec);
        self.cleanup_runtime(&id);
        if live.started.elapsed() > Duration::from_secs(10) {
            self.restarts.remove(&id);
        }
        for w in std::mem::take(&mut live.waiters) {
            let resp = match w.kind {
                WaitKind::Wait => Response::Exit { status: code },
                WaitKind::Stop => Response::Ok,
            };
            self.send(w.conn, &resp);
        }
        let stopped = self.user_stopped.remove(&id);
        let attempts = self.restarts.get(&id).copied().unwrap_or(0);
        if should_restart(live.spec.restart, code, attempts, stopped) {
            self.restarts.insert(id, attempts + 1);
            self.schedule.push((Instant::now() + restart_delay(attempts), Task::Relaunch(id)));
        } else if !live.spec.persistent {
            self.schedule.push((Instant::now() + EXITED_GRACE, Task::RemoveExited(id)));
        }
        self.fw_dirty = true;
    }

    fn reap_idle(&mut self, id: ContainerId) {
        if self.live.contains_key(&id) {
            let _ = self.cgroups.kill(&id);
            self.reap_blocking(id);
        }
        let _ = self.purge(&id);
    }

    fn op_commit(&mut self, target: &str, image: &str) -> Result<String> {
        let id = self.resolve(target)?;
        let spec = self.store.get(&id)?;
        let RootSource::Oci { digest, layers } = &spec.root else {
            return Err(Error::Invalid("only containers started from an image (run --image) can be committed".into()));
        };
        if self.root_mode == RootMode::BindRo {
            return Err(Error::Invalid("this host has no writable layer to commit (root_mode is bind-ro)".into()));
        }
        let (upper, _work) = self.upper_and_work_dirs(&id, &spec);
        if !upper.is_dir() {
            return Err(Error::NotFound(format!("no writable layer found for {target}")));
        }
        let store = collocate_image::config::ImageStore::new(&self.cfg.state_dir);
        let diff_id = collocate_image::commit::commit_upper_to_store(&upper, &store)?;
        let source = store.list()?.into_iter().find(|m| &m.digest == digest);
        let kind = source.as_ref().map(|m| m.kind).unwrap_or(spec.image_kind);
        let config = source.map(|m| m.config).unwrap_or_default();
        let mut new_layers = layers.clone();
        new_layers.push(diff_id);
        let meta_digest = collocate_image::commit::synthetic_digest(&new_layers, &config)?;
        let meta = collocate_image::config::ImageMeta { name: image.to_string(), digest: meta_digest.clone(), layers: new_layers, config, kind };
        store.put(&meta)?;
        Ok(meta_digest)
    }

    fn purge(&mut self, id: &ContainerId) -> Result<()> {
        self.store.remove(id)?;
        let _ = self.store.purge_trash();
        self.ipam.release(&id.to_string());
        self.remove_runtime_dirs(id);
        Ok(())
    }

    fn flush_firewall(&mut self) {
        if !self.fw_dirty {
            return;
        }
        self.fw_dirty = false;
        let script = render(&self.ruleset());
        let child = Command::new("nft").args(["-f", "-"]).stdin(Stdio::piped()).stderr(Stdio::piped()).spawn();
        match child {
            Ok(mut c) => {
                if let Some(mut stdin) = c.stdin.take() {
                    let _ = stdin.write_all(script.as_bytes());
                }
                match c.wait_with_output() {
                    Ok(o) if !o.status.success() => log!("nft failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => log!("nft failed: {e}"),
                    _ => {}
                }
            }
            Err(e) => log!("cannot run nft: {e}"),
        }
    }

    fn backends(&self, lb: &LbSpec) -> Vec<Backend> {
        let mut out: Vec<(ContainerId, Backend)> = self
            .live
            .values()
            .filter(|l| {
                l.spec.labels.project.as_deref() == Some(lb.project.as_str())
                    && l.spec.labels.service.as_deref() == Some(lb.backend_service.as_str())
            })
            .filter(|l| !l.stop_requested && l.tracker.as_ref().is_none_or(|t| t.state() == HealthState::Healthy))
            .filter_map(|l| Some((l.spec.id, Backend { addr: l.spec.net.addr?, port: lb.backend_port, weight: 1 })))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out.into_iter().map(|(_, b)| b).collect()
    }

    fn ruleset(&self) -> Ruleset {
        let mut containers: Vec<ContainerPorts> = self
            .live
            .values()
            .filter(|l| !l.spec.net.publish.is_empty())
            .filter_map(|l| Some(ContainerPorts { id: l.spec.id, addr: l.spec.net.addr?, publish: l.spec.net.publish.clone() }))
            .collect();
        containers.sort_by_key(|c| c.id);
        let lbs = self
            .lbs
            .values()
            .filter_map(|lb| {
                Some(LbRule {
                    name: format!("{}/{}", lb.project, lb.name),
                    vip: lb.vip?,
                    proto: lb.proto,
                    listen: lb.listen,
                    publish: lb.publish.clone(),
                    algorithm: lb.algorithm,
                    backends: self.backends(lb),
                    on_no_backends: lb.on_no_backends,
                })
            })
            .collect();
        Ruleset {
            bridge: self.cfg.bridge.clone(),
            subnet: Subnet::parse(&self.cfg.subnet).unwrap_or_else(|_| Subnet::parse("172.30.0.0/16").expect("default subnet")),
            containers,
            lbs,
        }
    }

    fn info(&self) -> String {
        let probe = collocate_sys::probe::probe();
        let checks: Vec<serde_json::Value> =
            probe.checks.iter().map(|c| serde_json::json!({ "name": c.name, "ok": c.ok, "detail": c.detail })).collect();
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "kernel": fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim(),
            "root_mode": match self.root_mode {
                RootMode::Overlay => "overlay",
                RootMode::FuseOverlay => "fuse-overlay",
                RootMode::BindRo => "bind-ro",
            },
            "subnet": self.cfg.subnet,
            "bridge": self.cfg.bridge,
            "node_uuid": self.node.node_uuid,
            "node": self.node.name,
            "running": self.live.len(),
            "checks": checks,
        })
        .to_string()
    }

    fn container_info(&self, spec: &Spec) -> ContainerInfo {
        let live = self.live.get(&spec.id);
        let state = match live {
            Some(_) => State::Running,
            None if spec.exit_status.is_some() => State::Exited,
            None => State::Stopped,
        };
        ContainerInfo {
            id: spec.id,
            name: spec.name.clone(),
            state,
            pid: live.map(|l| l.pid as u32),
            address: spec.net.addr,
            project: spec.labels.project.clone(),
            service: spec.labels.service.clone(),
            health: live.and_then(|l| l.tracker.as_ref().map(|t| t.state())),
            revision: spec.labels.revision.clone(),
            series: match &spec.root {
                RootSource::Base { series, .. } => Some(series.version().to_string()),
                RootSource::Oci { .. } => None,
            },
            published: spec.net.publish.iter().map(ToString::to_string).collect(),
            image_kind: match &spec.root {
                RootSource::Base { .. } => None,
                RootSource::Oci { .. } => Some(spec.image_kind),
            },
        }
    }

    fn handle(&mut self, conn: u64, req: Request) -> Result<Option<Response>> {
        match req {
            Request::Run(spec) => self.op_run(*spec).map(|id| Some(Response::Id { id })),
            Request::Start { target } => {
                let id = self.resolve(&target)?;
                if self.live.contains_key(&id) {
                    return Err(Error::Conflict(format!("{target} is already running")));
                }
                self.user_stopped.remove(&id);
                self.launch(self.store.get(&id)?)?;
                Ok(Some(Response::Ok))
            }
            Request::Stop { target, timeout_secs } => self.op_stop(conn, &target, timeout_secs),
            Request::Restart { target, timeout_secs } => {
                let id = self.resolve(&target)?;
                if self.live.contains_key(&id) {
                    return Err(Error::Invalid("restart is composed of stop and start by the client".into()));
                }
                let _ = timeout_secs;
                self.launch(self.store.get(&id)?)?;
                Ok(Some(Response::Ok))
            }
            Request::Kill { target, signal } => {
                let id = self.resolve(&target)?;
                let live = self.live.get(&id).ok_or_else(|| Error::Conflict(format!("{target} is not running")))?;
                if signal == libc::SIGKILL {
                    io(self.cgroups.kill(&id))?;
                } else {
                    io(send_signal(&live.pidfd, signal))?;
                }
                Ok(Some(Response::Ok))
            }
            Request::Rm { target, force, keep_data: _ } => {
                let id = self.resolve(&target)?;
                if self.live.contains_key(&id) {
                    if !force {
                        return Err(Error::Conflict(format!("{target} is running; stop it or use force")));
                    }
                    io(self.cgroups.kill(&id))?;
                    self.reap_blocking(id);
                }
                self.purge(&id)?;
                Ok(Some(Response::Ok))
            }
            Request::Wait { target } => {
                let id = self.resolve(&target)?;
                match self.live.get_mut(&id) {
                    Some(l) => {
                        l.waiters.push(Waiter { conn, kind: WaitKind::Wait });
                        Ok(None)
                    }
                    None => Ok(Some(Response::Exit { status: self.store.get(&id)?.exit_status.unwrap_or(0) })),
                }
            }
            Request::Logs { target, tail, offset, source, services } => {
                let id = self.resolve(&target)?;
                let spec = self.store.get(&id)?;
                self.op_logs(&spec, tail, offset, source, &services).map(Some)
            }
            Request::Exec { target, argv, env, user, workdir, tty: _, timeout_secs } => {
                self.op_exec(conn, ExecArgs { target, argv, env, user, workdir, timeout_secs })
            }
            Request::ExecProbe { target, argv, timeout_secs } => self.op_exec_probe(conn, &target, argv, timeout_secs),
            Request::Commit { target, image } => self.op_commit(&target, &image).map(|digest| Some(Response::Text { text: digest })),
            Request::Ps { all, project } => {
                let list = self
                    .store
                    .load_all()?
                    .iter()
                    .filter(|s| project.is_none() || s.labels.project == project)
                    .map(|s| self.container_info(s))
                    .filter(|c| all || c.state == State::Running)
                    .collect();
                Ok(Some(Response::Containers(list)))
            }
            Request::Stats { project } => {
                let mut out = Vec::new();
                for l in self.live.values() {
                    if project.is_some() && l.spec.labels.project != project {
                        continue;
                    }
                    if let Ok(s) = self.cgroups.stats(&l.spec.id) {
                        out.push(ContainerStats {
                            id: l.spec.id,
                            name: l.spec.name.clone(),
                            project: l.spec.labels.project.clone(),
                            service: l.spec.labels.service.clone(),
                            cpu_usage_usec: s.cpu_usage_usec,
                            memory_current: s.memory_current,
                            memory_max: s.memory_max,
                            pids: s.pids_current,
                            cpu_limit_milli: l.spec.limits.cpus_milli,
                        });
                    }
                }
                out.sort_by_key(|s| s.id);
                Ok(Some(Response::Stats(out)))
            }
            Request::SecretEnsure { project, name, generate, length } => {
                self.secrets.ensure(&project, &name, &generate, length)?;
                Ok(Some(Response::Ok))
            }
            Request::SecretReveal { project, name } => Ok(Some(Response::Text { text: self.secrets.reveal(&project, &name)? })),
            Request::SecretSet { project, name, value } => {
                self.secrets.set(&project, &name, &value)?;
                Ok(Some(Response::Ok))
            }
            Request::SecretList { project } => Ok(Some(Response::Names(self.secrets.list(project.as_deref())?))),
            Request::SecretRemove { project, name } => {
                self.secrets.remove(&project, &name)?;
                Ok(Some(Response::Ok))
            }
            Request::LbSet { lb } => self.op_lb_set(lb).map(|_| Some(Response::Ok)),
            Request::LbRemove { project, name } => {
                let key = lb_key(&project, &name);
                let lb = self.lbs.remove(&key).ok_or_else(|| Error::NotFound(format!("load balancer {project}/{name}")))?;
                let _ = fs::remove_file(self.lb_dir().join(format!("{key}.json")));
                if let Some(vip) = lb.vip {
                    let _ = self.vip_cmd("del", vip).run();
                }
                self.ipam.release(&format!("vip:{project}/{name}"));
                self.fw_dirty = true;
                Ok(Some(Response::Ok))
            }
            Request::LbList => {
                let list = self
                    .lbs
                    .values()
                    .map(|lb| LbStatus {
                        spec: lb.clone(),
                        vip: lb.vip.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
                        backends: self.backends(lb).iter().map(|b| format!("{}:{}", b.addr, b.port)).collect(),
                    })
                    .collect();
                Ok(Some(Response::Lbs(list)))
            }
            Request::Info => Ok(Some(Response::Text { text: self.info() })),
            Request::Shutdown => {
                self.shutdown = true;
                Ok(Some(Response::Ok))
            }
        }
    }

    fn op_lb_set(&mut self, mut lb: LbSpec) -> Result<()> {
        if lb.vip.is_none() {
            lb.vip = Some(self.ipam.vip(&lb.project, &lb.name)?);
        } else if let Some(v) = lb.vip {
            self.ipam.reserve(v, &format!("vip:{}/{}", lb.project, lb.name))?;
        }
        let vip = lb.vip.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
        self.vip_cmd("add", vip).run_allowing(Some("File exists"))?;
        io(fs::create_dir_all(self.lb_dir()))?;
        let key = lb_key(&lb.project, &lb.name);
        io(fs::write(self.lb_dir().join(format!("{key}.json")), serde_json::to_vec_pretty(&lb)?))?;
        self.lbs.insert(key, lb);
        self.fw_dirty = true;
        Ok(())
    }

    fn op_run(&mut self, mut spec: Spec) -> Result<ContainerId> {
        if spec.name.is_empty() {
            spec.name = format!("c-{}", &spec.id.to_string()[..8]);
            spec.hostname = spec.name.clone();
        }
        self.cfg.apply_defaults(&mut spec.limits)?;
        spec.validate()?;
        if let RootSource::Base { series, build_id } = &spec.root {
            if build_id == "latest" {
                let lowers = self.lowers(&spec.root)?;
                let build = lowers[0].parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                spec.root = RootSource::Base { series: *series, build_id: build };
            }
        }
        self.store.create(&spec)?;
        let id = spec.id;
        if let Err(e) = self.launch(spec) {
            let _ = self.purge(&id);
            return Err(e);
        }
        Ok(id)
    }

    fn drain_for(&self, spec: &Spec) -> Duration {
        self.lbs
            .values()
            .filter(|lb| {
                spec.labels.project.as_deref() == Some(lb.project.as_str())
                    && spec.labels.service.as_deref() == Some(lb.backend_service.as_str())
            })
            .map(|lb| lb.drain_secs)
            .max()
            .map_or(Duration::ZERO, Duration::from_secs)
    }

    fn op_stop(&mut self, conn: u64, target: &str, timeout: Option<u64>) -> Result<Option<Response>> {
        let id = self.resolve(target)?;
        let drain = match self.live.get(&id) {
            Some(l) => self.drain_for(&l.spec),
            None => return Ok(Some(Response::Ok)),
        };
        let Some(live) = self.live.get_mut(&id) else { return Ok(Some(Response::Ok)) };
        live.waiters.push(Waiter { conn, kind: WaitKind::Stop });
        if !live.stop_requested {
            live.stop_requested = true;
            let now = Instant::now();
            let grace = Duration::from_secs(timeout.unwrap_or(live.spec.process.stop_timeout_secs));
            live.signal_at = Some(now + drain);
            live.kill_at = Some(now + drain + grace);
            self.user_stopped.insert(id);
            self.fw_dirty = true;
        }
        Ok(None)
    }

    fn captured_logs(&self, spec: &Spec, tail: Option<usize>, offset: Option<u64>) -> Response {
        let bytes = fs::read(self.log_path(spec)).unwrap_or_default();
        let total = bytes.len() as u64;
        let text = match offset {
            Some(o) => String::from_utf8_lossy(&bytes[(o.min(total)) as usize..]).into_owned(),
            None => {
                let all = String::from_utf8_lossy(&bytes).into_owned();
                match tail {
                    Some(n) => {
                        let lines: Vec<&str> = all.lines().collect();
                        let from = lines.len().saturating_sub(n);
                        let mut t = lines[from..].join("\n");
                        if !t.is_empty() {
                            t.push('\n');
                        }
                        t
                    }
                    None => all,
                }
            }
        };
        Response::Log { data: text, next_offset: total, source: LogSource::Captured }
    }

    fn pebble_client(&self, id: &ContainerId, spec: &Spec) -> Option<collocate_pebble::Client> {
        let l = self.live.get(id)?;
        let socket = format!("/proc/{}/root{}", l.pid, spec.pebble_socket());
        Some(collocate_pebble::Client::new(socket, CONNECT_TIMEOUT))
    }

    fn op_logs(&self, spec: &Spec, tail: Option<usize>, offset: Option<u64>, source: LogSource, services: &[String]) -> Result<Response> {
        let want_pebble = match source {
            LogSource::Captured => false,
            LogSource::Pebble => true,
            LogSource::Auto => spec.image_kind == ImageKind::Pebble || !services.is_empty(),
        };
        if !want_pebble {
            return Ok(self.captured_logs(spec, tail, offset));
        }
        if spec.image_kind != ImageKind::Pebble {
            return Err(Error::Invalid(format!("{} is not a rock, so it has no Pebble services", spec.name)));
        }
        let fetched = self
            .pebble_client(&spec.id, spec)
            .ok_or_else(|| Error::Conflict(format!("{} is not running", spec.name)))
            .and_then(|c| c.logs(services, if offset.is_some() { None } else { tail }).map_err(|e| Error::Unreachable(e.to_string())));
        let entries = match fetched {
            Ok(entries) => entries,
            Err(_) if source == LogSource::Auto && services.is_empty() => return Ok(self.captured_logs(spec, tail, offset)),
            Err(e) => return Err(e),
        };
        let after = offset.unwrap_or(0);
        let mut next = after;
        let mut data = String::new();
        for e in entries.iter().filter(|e| e.nanos() > after) {
            data.push_str(&e.render());
            next = next.max(e.nanos());
        }
        Ok(Response::Log { data, next_offset: next, source: LogSource::Pebble })
    }

    fn op_exec(&mut self, conn: u64, args: ExecArgs) -> Result<Option<Response>> {
        let id = self.resolve(&args.target)?;
        let fds: Vec<OwnedFd> = {
            let c = self.conns.get_mut(&conn).ok_or_else(|| Error::Internal("connection vanished".into()))?;
            if c.fds.len() < 3 {
                return Err(Error::Invalid("exec needs stdin, stdout and stderr descriptors".into()));
            }
            c.fds.drain(..3).collect()
        };
        let live = self.live.get(&id).ok_or_else(|| Error::Conflict(format!("{} is not running", args.target)))?;
        let req = ExecRequest {
            init_pid: live.pid,
            init_pidfd: live.pidfd.as_raw_fd(),
            cgroup_procs: self.cgroups.container_dir(&id).join("cgroup.procs"),
            spec: &live.spec,
            argv: args.argv,
            env: args.env,
            user: args.user,
            workdir: args.workdir,
            stdio: [fds[0].as_raw_fd(), fds[1].as_raw_fd(), fds[2].as_raw_fd()],
        };
        let (_pid, pidfd) = spawn_exec(&req)?;
        let idle_timeout = live.spec.idle_timeout_secs;
        if let Some(l) = self.live.get_mut(&id) {
            l.last_activity = Instant::now();
        }
        self.schedule_idle_reap(id, idle_timeout);
        let token = self.token();
        io(self.epoll.add(pidfd.as_raw_fd(), token, Events::READABLE))?;
        let deadline = args.timeout_secs.map(|t| Instant::now() + Duration::from_secs(t.max(1)));
        self.execs.insert(token, ExecProc { pidfd, conn, deadline });
        Ok(None)
    }

    fn reap_blocking(&mut self, id: ContainerId) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let done = match self.live.get(&id) {
                Some(l) => match wait(&l.pidfd, true) {
                    Ok(Some(st)) => Some(Some(st.as_code())),
                    Ok(None) => None,
                    Err(_) => Some(None),
                },
                None => return,
            };
            if let Some(code) = done {
                self.on_exit(id, code);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.on_exit(id, None);
    }

    fn send(&mut self, conn: u64, resp: &Response) {
        let Some(c) = self.conns.get_mut(&conn) else { return };
        let Ok(payload) = serde_json::to_vec(resp) else { return };
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        let mut sent = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while sent < frame.len() {
            match std::io::Write::write(&mut c.stream, &frame[sent..]) {
                Ok(n) => sent += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(_) => {
                    self.drop_conn(conn);
                    return;
                }
            }
        }
    }

    fn drop_conn(&mut self, conn: u64) {
        if let Some(c) = self.conns.remove(&conn) {
            let _ = self.epoll.remove(c.stream.as_raw_fd());
        }
    }

    fn accept(&mut self) {
        while let Ok((stream, _)) = self.listener.accept() {
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            let token = self.token();
            if self.epoll.add(stream.as_raw_fd(), token, Events::READABLE).is_ok() {
                self.conns.insert(token, Conn { stream, rbuf: Vec::new(), fds: VecDeque::new() });
            }
        }
    }

    fn on_conn(&mut self, token: u64) {
        let mut closed = false;
        loop {
            let Some(c) = self.conns.get_mut(&token) else { return };
            match recv_fd(&c.stream, 65536, 4) {
                Ok((data, fds)) => {
                    if data.is_empty() && fds.is_empty() {
                        closed = true;
                        break;
                    }
                    c.rbuf.extend_from_slice(&data);
                    c.fds.extend(fds);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        loop {
            let Some(c) = self.conns.get_mut(&token) else { return };
            if c.rbuf.len() < 4 {
                break;
            }
            let len = u32::from_be_bytes([c.rbuf[0], c.rbuf[1], c.rbuf[2], c.rbuf[3]]) as usize;
            if len > MAX_FRAME {
                closed = true;
                break;
            }
            if c.rbuf.len() < 4 + len {
                break;
            }
            let payload: Vec<u8> = c.rbuf.drain(..4 + len).skip(4).collect();
            let resp = match serde_json::from_slice::<Request>(&payload) {
                Ok(req) => match self.handle(token, req) {
                    Ok(Some(r)) => Some(r),
                    Ok(None) => None,
                    Err(e) => Some(Response::error(&e)),
                },
                Err(e) => Some(Response::error(&Error::Invalid(format!("bad request: {e}")))),
            };
            if let Some(r) = resp {
                self.send(token, &r);
            }
        }
        if closed {
            let pending = self.live.values().any(|l| l.waiters.iter().any(|w| w.conn == token))
                || self.execs.values().any(|e| e.conn == token)
                || self.probes.values().any(|p| matches!(p.origin, ProbeOrigin::Client(c) if c == token));
            if !pending {
                self.drop_conn(token);
            }
        }
    }

    fn on_pidfd(&mut self, token: u64) {
        if let Some(id) = self.pid_tokens.get(&token).copied() {
            let outcome = match self.live.get(&id) {
                Some(l) if l.child => match wait(&l.pidfd, true) {
                    Ok(Some(st)) => Some(Some(st.as_code())),
                    Ok(None) => None,
                    Err(_) => Some(None),
                },
                Some(_) => Some(None),
                None => None,
            };
            if let Some(code) = outcome {
                self.on_exit(id, code);
            }
        } else if let Some(ex) = self.execs.get(&token) {
            if let Ok(Some(st)) = wait(&ex.pidfd, true) {
                let conn = ex.conn;
                let _ = self.epoll.remove(ex.pidfd.as_raw_fd());
                self.execs.remove(&token);
                let code = match st {
                    ExitStatus::Code(c) => c,
                    ExitStatus::Signal(s) => 128 + s,
                };
                self.send(conn, &Response::Exit { status: code });
            }
        } else if let Some(p) = self.probes.get(&token) {
            if let Ok(Some(st)) = wait(&p.pidfd, true) {
                let origin = p.origin;
                let ok = matches!(st, ExitStatus::Code(0));
                let _ = self.epoll.remove(p.pidfd.as_raw_fd());
                self.probes.remove(&token);
                match origin {
                    ProbeOrigin::Health(id) => self.record_health(id, ok),
                    ProbeOrigin::Client(conn) => {
                        let code = match st {
                            ExitStatus::Code(c) => c,
                            ExitStatus::Signal(s) => 128 + s,
                        };
                        self.send(conn, &Response::Exit { status: code });
                    }
                }
            }
        }
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        let mut due = Vec::new();
        let mut i = 0;
        while i < self.schedule.len() {
            if self.schedule[i].0 <= now {
                due.push(self.schedule.remove(i).1);
            } else {
                i += 1;
            }
        }
        for task in due {
            match task {
                Task::Relaunch(id) => match self.store.get(&id) {
                    Ok(spec) if !self.live.contains_key(&id) => {
                        if let Err(e) = self.launch(spec) {
                            log!("restart of {id} failed: {e}");
                        }
                    }
                    _ => {}
                },
                Task::RemoveExited(id) => {
                    if !self.live.contains_key(&id) && self.store.get(&id).is_ok_and(|s| !s.persistent) {
                        let _ = self.purge(&id);
                    }
                }
                Task::ReapIdle(id) => {
                    let due = self.live.get(&id).is_some_and(|l| {
                        l.spec.idle_timeout_secs.is_some_and(|t| now.duration_since(l.last_activity) >= Duration::from_secs(t.max(1)))
                    });
                    if due {
                        self.reap_idle(id);
                    }
                }
            }
        }
        let ids: Vec<ContainerId> = self.live.keys().copied().collect();
        for id in ids {
            let Some(l) = self.live.get_mut(&id) else { continue };
            if l.signal_at.is_some_and(|t| t <= now) {
                l.signal_at = None;
                let _ = send_signal(&l.pidfd, l.spec.process.stop_signal);
            }
            if l.kill_at.is_some_and(|t| t <= now) {
                l.kill_at = None;
                let _ = self.cgroups.kill(&id);
            }
        }
        let overdue: Vec<u64> = self.probes.iter().filter(|(_, p)| p.deadline <= now).map(|(t, _)| *t).collect();
        for token in overdue {
            if let Some(p) = self.probes.get(&token) {
                let _ = send_signal(&p.pidfd, libc::SIGKILL);
            }
        }
        let overdue_execs: Vec<u64> = self.execs.iter().filter(|(_, e)| e.deadline.is_some_and(|d| d <= now)).map(|(t, _)| *t).collect();
        for token in overdue_execs {
            if let Some(e) = self.execs.get(&token) {
                let _ = send_signal(&e.pidfd, libc::SIGKILL);
            }
        }
        if now.duration_since(self.last_health) >= HEALTH_TICK {
            self.last_health = now;
            self.check_health(now);
        }
    }

    fn check_health(&mut self, now: Instant) {
        let in_flight: HashSet<ContainerId> =
            self.probes.values().filter_map(|p| if let ProbeOrigin::Health(id) = p.origin { Some(id) } else { None }).collect();
        let mut due: Vec<(ContainerId, HealthKind, Option<std::net::Ipv4Addr>, u64)> = Vec::new();
        for (id, l) in self.live.iter_mut() {
            let (Some(_), Some(hc)) = (l.tracker.as_ref(), l.spec.healthcheck.as_ref()) else {
                continue;
            };
            let addr = l.spec.net.addr;
            if addr.is_none() && matches!(hc.kind, HealthKind::Tcp { .. } | HealthKind::Http { .. }) {
                continue;
            }
            if now.duration_since(l.last_check) < Duration::from_secs(hc.interval_secs.max(1)) {
                continue;
            }
            if matches!(hc.kind, HealthKind::Exec { .. }) && in_flight.contains(id) {
                continue;
            }
            l.last_check = now;
            due.push((*id, hc.kind.clone(), addr, hc.timeout_secs.max(1)));
        }
        for (id, kind, addr, timeout_secs) in due {
            let timeout = Duration::from_secs(timeout_secs).min(CONNECT_TIMEOUT);
            match (kind, addr) {
                (HealthKind::Tcp { port }, Some(addr)) => {
                    let ok = tcp_check(addr, port, timeout);
                    self.record_health(id, ok);
                }
                (HealthKind::Http { port, path }, Some(addr)) => {
                    let ok = http_check(addr, port, &path, timeout);
                    self.record_health(id, ok);
                }
                (HealthKind::Tcp { .. } | HealthKind::Http { .. }, None) => {}
                (HealthKind::Exec { argv }, _) => self.spawn_health_exec(id, argv, now + Duration::from_secs(timeout_secs)),
                (HealthKind::Pebble { level }, _) => {
                    let problem = self.pebble_health(id, level.as_deref(), timeout);
                    let before = self.health_state(id);
                    self.record_health(id, problem.is_none());
                    if let Some(p) = problem.filter(|_| self.health_state(id) != before) {
                        log!("{}: {p}", self.live.get(&id).map_or("", |l| l.spec.name.as_str()));
                    }
                }
            }
        }
    }

    fn health_state(&self, id: ContainerId) -> Option<HealthState> {
        self.live.get(&id)?.tracker.as_ref().map(|t| t.state())
    }

    fn pebble_health(&self, id: ContainerId, level: Option<&str>, timeout: Duration) -> Option<String> {
        let Some(l) = self.live.get(&id) else { return Some("container is gone".into()) };
        let socket = format!("/proc/{}/root{}", l.pid, l.spec.pebble_socket());
        match collocate_pebble::Client::new(socket, timeout).health(level) {
            Ok(h) if h.healthy => None,
            Ok(h) => Some(format!("pebble reports {}", h.problems.join("; "))),
            Err(e) => Some(e.to_string()),
        }
    }

    fn record_health(&mut self, id: ContainerId, ok: bool) {
        let Some(l) = self.live.get_mut(&id) else { return };
        let Some(tracker) = l.tracker.as_mut() else { return };
        if tracker.record(ok).is_some() {
            let state = tracker.state();
            if let Ok(Some(mut rt)) = self.store.read_runtime(&id) {
                rt.health = Some(state);
                let _ = self.store.write_runtime(&id, &rt);
            }
            self.fw_dirty = true;
        }
    }

    fn spawn_health_exec(&mut self, id: ContainerId, argv: Vec<String>, deadline: Instant) {
        if !self.spawn_probe(id, argv, deadline, ProbeOrigin::Health(id)) {
            self.record_health(id, false);
        }
    }

    fn spawn_probe(&mut self, id: ContainerId, argv: Vec<String>, deadline: Instant, origin: ProbeOrigin) -> bool {
        let Some(l) = self.live.get(&id) else { return false };
        let Ok(devnull) = fs::OpenOptions::new().read(true).write(true).open("/dev/null") else { return false };
        let fd = devnull.as_raw_fd();
        let req = ExecRequest {
            init_pid: l.pid,
            init_pidfd: l.pidfd.as_raw_fd(),
            cgroup_procs: self.cgroups.container_dir(&id).join("cgroup.procs"),
            spec: &l.spec,
            argv,
            env: Vec::new(),
            user: None,
            workdir: None,
            stdio: [fd, fd, fd],
        };
        let Ok((_pid, pidfd)) = spawn_exec(&req) else { return false };
        let token = self.token();
        if self.epoll.add(pidfd.as_raw_fd(), token, Events::READABLE).is_ok() {
            self.probes.insert(token, Probe { pidfd, origin, deadline });
            true
        } else {
            false
        }
    }

    fn op_exec_probe(&mut self, conn: u64, target: &str, argv: Vec<String>, timeout_secs: u64) -> Result<Option<Response>> {
        let id = self.resolve(target)?;
        let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
        if self.spawn_probe(id, argv, deadline, ProbeOrigin::Client(conn)) {
            Ok(None)
        } else {
            Err(Error::Internal(format!("could not start probe in {target}")))
        }
    }

    fn arm_timer(&self) {
        let now = Instant::now();
        let mut next = self.last_health + HEALTH_TICK;
        for (t, _) in &self.schedule {
            next = next.min(*t);
        }
        for l in self.live.values() {
            for t in [l.signal_at, l.kill_at].into_iter().flatten() {
                next = next.min(t);
            }
        }
        for p in self.probes.values() {
            next = next.min(p.deadline);
        }
        for e in self.execs.values() {
            if let Some(d) = e.deadline {
                next = next.min(d);
            }
        }
        let _ = self.timer.arm(next.saturating_duration_since(now));
    }

    pub fn run(&mut self) -> Result<()> {
        while !self.shutdown {
            self.arm_timer();
            let ready = io(self.epoll.wait(None))?;
            for r in ready {
                match r.token {
                    LISTENER => self.accept(),
                    TIMER => {
                        let _ = self.timer.consume();
                        self.on_timer();
                    }
                    SIGNALS => {
                        while let Ok(Some(sig)) = self.signals.read() {
                            log!("received signal {sig}, shutting down");
                            self.shutdown = true;
                        }
                    }
                    t if self.conns.contains_key(&t) => self.on_conn(t),
                    t => self.on_pidfd(t),
                }
            }
            self.flush_firewall();
        }
        let _ = fs::remove_file(self.cfg.socket());
        let _ = &self.listener;
        Ok(())
    }
}
