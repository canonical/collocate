use crate::env::build_env;
use crate::mountplan::{mount_plan, Extras, MountOp};
use crate::overlay::OverlayPlan;
use crate::user::resolve_user;
use collocate_core::spec::Spec;
use collocate_core::{Error, Result};
use collocate_net::netcfg::{in_ns_config, move_to_ns, teardown_veth, veth_setup};
use collocate_sys::caps::CapSet;
use collocate_sys::clone::{clone3, CloneFlags, Forked};
use collocate_sys::fdpass::{recv_fd, send_fd, socketpair_seqpacket};
use collocate_sys::misc;
use collocate_sys::mount;
use collocate_sys::pidfd::send_signal;
use std::convert::Infallible;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMode {
    Overlay,
    FuseOverlay,
    BindRo,
}

#[derive(Debug, Clone)]
pub struct NetSetup {
    pub bridge: String,
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
}

pub struct LaunchRequest<'a> {
    pub spec: &'a Spec,
    pub root_mode: RootMode,
    pub lowers: Vec<PathBuf>,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub staging: PathBuf,
    pub extras: Extras,
    pub cgroup_dir: PathBuf,
    pub log_path: PathBuf,
    pub net: Option<NetSetup>,
}

pub struct Started {
    pub pid: i32,
    pub pidfd: OwnedFd,
    handshake: OwnedFd,
}

impl Started {
    pub fn confirm(&self, timeout: Duration) -> Result<()> {
        let mut pfd = libc::pollfd { fd: self.handshake.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let rc = loop {
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break rc;
            }
        };
        if rc == 0 {
            let _ = send_signal(&self.pidfd, libc::SIGKILL);
            return Err(Error::Timeout("container setup".into()));
        }
        let (data, _) = recv_fd(&self.handshake, 4096, 0)?;
        match data.first() {
            None => Ok(()),
            Some(b'E') => Err(Error::Internal(String::from_utf8_lossy(&data[1..]).into_owned())),
            Some(_) => Err(Error::Internal("unexpected handshake message".into())),
        }
    }
}

struct Prepared {
    ops: Vec<MountOp>,
    root_mode: RootMode,
    lowers: Vec<PathBuf>,
    upper: PathBuf,
    work: PathBuf,
    staging: PathBuf,
    read_only_rootfs: bool,
    volatile: bool,
    hostname: String,
    user: String,
    workdir: String,
    ulimits: Vec<(String, u64, u64)>,
    caps: CapSet,
    argv: Vec<CString>,
    envp: Vec<CString>,
}

fn cstring(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::InvalidSpec(format!("string contains NUL: {s:?}")))
}

impl Prepared {
    fn new(req: &LaunchRequest) -> Result<Prepared> {
        let spec = req.spec;
        let mut argv = vec![cstring("/.collocate/init")?, cstring("--")?];
        for a in &spec.process.argv {
            argv.push(cstring(a)?);
        }
        let envp = build_env(spec).iter().map(|e| cstring(e)).collect::<Result<Vec<_>>>()?;
        Ok(Prepared {
            ops: mount_plan(spec, &req.extras)?,
            root_mode: req.root_mode,
            lowers: req.lowers.clone(),
            upper: req.upper.clone(),
            work: req.work.clone(),
            staging: req.staging.clone(),
            read_only_rootfs: spec.read_only_rootfs,
            volatile: !spec.persistent,
            hostname: spec.hostname.clone(),
            user: spec.process.user.clone(),
            workdir: spec.process.workdir.clone(),
            ulimits: spec.limits.ulimits.iter().map(|u| (u.name.clone(), u.soft, u.hard)).collect(),
            caps: CapSet::with_policy(&spec.caps.add, &spec.caps.drop)?,
            argv,
            envp,
        })
    }
}

pub fn spawn(req: &LaunchRequest) -> Result<Started> {
    let prepared = Prepared::new(req)?;
    let (parent_sock, child_sock) = socketpair_seqpacket()?;
    let cgroup = File::open(&req.cgroup_dir)?;
    let log = OpenOptions::new().create(true).append(true).mode(0o640).open(&req.log_path)?;
    if let Some(n) = &req.net {
        for c in veth_setup(&req.spec.id, &n.bridge) {
            c.run_allowing(Some("File exists"))?;
        }
    }
    let flags = CloneFlags::NEWNS | CloneFlags::NEWPID | CloneFlags::NEWIPC | CloneFlags::NEWUTS | CloneFlags::NEWNET;
    match clone3(flags, Some(&cgroup))? {
        Forked::Child => {
            drop(parent_sock);
            child_main(&prepared, &child_sock, &log)
        }
        Forked::Parent { pid, pidfd } => {
            drop(child_sock);
            if let Some(n) = &req.net {
                let setup = || -> Result<()> {
                    move_to_ns(&req.spec.id, pid).run()?;
                    for c in in_ns_config(pid, &req.spec.id, n.addr, n.prefix, n.gateway, req.spec.net.ping_group) {
                        if c.0.iter().any(|a| a.contains("ping_group_range")) {
                            if let Err(e) = c.run() {
                                eprintln!("collocate: unprivileged ping not enabled: {e}");
                            }
                        } else {
                            c.run()?;
                        }
                    }
                    Ok(())
                };
                if let Err(e) = setup() {
                    let _ = send_signal(&pidfd, libc::SIGKILL);
                    let _ = teardown_veth(&req.spec.id).run();
                    return Err(e);
                }
            }
            send_fd(&parent_sock, b"N", &[])?;
            Ok(Started { pid, pidfd, handshake: parent_sock })
        }
    }
}

fn child_main(p: &Prepared, sock: &OwnedFd, log: &File) -> ! {
    match child_run(p, sock, log) {
        Ok(never) => match never {},
        Err(msg) => {
            let _ = send_fd(sock, format!("E{msg}").as_bytes(), &[]);
            unsafe { libc::_exit(126) }
        }
    }
}

fn step<T>(name: &str, r: std::io::Result<T>) -> std::result::Result<T, String> {
    r.map_err(|e| format!("{name}: {e}"))
}

fn join(staging: &Path, dst: &str) -> PathBuf {
    staging.join(dst.trim_start_matches('/'))
}

fn ensure_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

fn ensure_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create(path).map(|_| ())
}

fn text(p: &Path) -> &str {
    p.to_str().unwrap_or("")
}

const NOSDX: u64 = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;

fn apply_op(staging: &Path, op: &MountOp) -> std::result::Result<(), String> {
    let name = format!("{op:?}");
    match op {
        MountOp::Proc => step(&name, mount::mount(Some("proc"), text(&join(staging, "/proc")), Some("proc"), NOSDX, None)),
        MountOp::SysfsRo => {
            step(&name, mount::mount(Some("sysfs"), text(&join(staging, "/sys")), Some("sysfs"), NOSDX | libc::MS_RDONLY, None))
        }
        MountOp::Cgroup2Ro => step(
            &name,
            mount::mount(Some("cgroup2"), text(&join(staging, "/sys/fs/cgroup")), Some("cgroup2"), NOSDX | libc::MS_RDONLY, None),
        ),
        MountOp::DevTmpfs => step(
            &name,
            mount::mount(
                Some("tmpfs"),
                text(&join(staging, "/dev")),
                Some("tmpfs"),
                libc::MS_NOSUID | libc::MS_STRICTATIME,
                Some("mode=755,size=65536k"),
            ),
        ),
        MountOp::DevNode(n) => {
            let target = join(staging, &format!("/dev/{n}"));
            step(&name, ensure_file(&target))?;
            step(&name, mount::bind(&format!("/dev/{n}"), text(&target), false))
        }
        MountOp::DevPts => {
            let target = join(staging, "/dev/pts");
            step(&name, ensure_dir(&target))?;
            step(
                &name,
                mount::mount(
                    Some("devpts"),
                    text(&target),
                    Some("devpts"),
                    libc::MS_NOSUID | libc::MS_NOEXEC,
                    Some("newinstance,ptmxmode=0666,mode=0620"),
                ),
            )?;
            step(&name, std::os::unix::fs::symlink("pts/ptmx", join(staging, "/dev/ptmx")))
        }
        MountOp::DevShm { size } => {
            let target = join(staging, "/dev/shm");
            step(&name, ensure_dir(&target))?;
            step(
                &name,
                mount::mount(
                    Some("shm"),
                    text(&target),
                    Some("tmpfs"),
                    libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                    Some(&format!("mode=1777,size={size}")),
                ),
            )
        }
        MountOp::RunTmpfs => step(
            &name,
            mount::mount(
                Some("tmpfs"),
                text(&join(staging, "/run")),
                Some("tmpfs"),
                libc::MS_NOSUID | libc::MS_NODEV,
                Some("mode=755,size=64m"),
            ),
        ),
        MountOp::Tmpfs { dst, size } => {
            let target = join(staging, dst);
            step(&name, ensure_dir(&target))?;
            let opts = match size {
                Some(s) => format!("mode=1777,size={s}"),
                None => "mode=1777".to_string(),
            };
            step(&name, mount::mount(Some("tmpfs"), text(&target), Some("tmpfs"), libc::MS_NOSUID | libc::MS_NODEV, Some(&opts)))
        }
        MountOp::Bind { src, dst, ro } => {
            let target = join(staging, dst);
            let is_dir = src.is_dir();
            step(&name, if is_dir { ensure_dir(&target) } else { ensure_file(&target) })?;
            step(&name, mount::bind(text(src), text(&target), is_dir))?;
            if *ro {
                step(&name, mount::mount(None, text(&target), None, libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY, None))?;
            }
            Ok(())
        }
        MountOp::MaskFile(p) => {
            let target = join(staging, p);
            if target.exists() {
                step(&name, mount::bind("/dev/null", text(&target), false))?;
            }
            Ok(())
        }
        MountOp::MaskDir(p) => {
            let target = join(staging, p);
            if target.is_dir() {
                step(&name, mount::mount(Some("tmpfs"), text(&target), Some("tmpfs"), NOSDX | libc::MS_RDONLY, Some("mode=755,size=4k")))?;
            }
            Ok(())
        }
        MountOp::ProcSysRo => {
            let target = join(staging, "/proc/sys");
            step(&name, mount::bind(text(&target), text(&target), false))?;
            step(&name, mount::mount(None, text(&target), None, libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY, None))
        }
    }
}

fn child_run(p: &Prepared, sock: &OwnedFd, log: &File) -> std::result::Result<Infallible, String> {
    step("unshare cgroup namespace", misc::unshare(u64::from(libc::CLONE_NEWCGROUP as u32)))?;
    step("make mounts private", mount::make_private_recursive("/"))?;

    let (msg, _) = step("await network", recv_fd(sock, 8, 0))?;
    if msg != b"N" {
        return Err("launcher aborted".into());
    }

    let devnull = step("open /dev/null", File::open("/dev/null"))?;
    step("stdin", misc::dup2(devnull.as_raw_fd(), 0))?;
    step("stdout", misc::dup2(log.as_raw_fd(), 1))?;
    step("stderr", misc::dup2(log.as_raw_fd(), 2))?;

    let staging = p.staging.as_path();
    match p.root_mode {
        RootMode::Overlay => {
            let plan = OverlayPlan { lowers: p.lowers.clone(), upper: p.upper.clone(), work: p.work.clone(), volatile: p.volatile };
            plan.mount(text(staging)).map_err(|e| format!("overlay mount: {e}"))?;
        }
        RootMode::BindRo => {
            let base = p.lowers.first().ok_or("no root filesystem")?;
            step("bind root", mount::bind(text(base), text(staging), false))?;
            step(
                "remount root read-only",
                mount::mount(None, text(staging), None, libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY, None),
            )?;
        }
        RootMode::FuseOverlay => {
            let merged = p.lowers.first().ok_or("no root filesystem")?;
            step("bind fuse-overlayfs root", mount::bind(text(merged), text(staging), false))?;
        }
    }

    for op in &p.ops {
        apply_op(staging, op)?;
    }
    if p.read_only_rootfs && p.root_mode != RootMode::BindRo {
        step("remount root read-only", mount::mount(None, text(staging), None, libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY, None))?;
    }

    step("chdir to new root", misc::chdir(text(staging)))?;
    step("pivot_root", mount::pivot_root(".", "."))?;
    step("detach old root", mount::umount_lazy("."))?;
    step("chdir /", misc::chdir("/"))?;

    step("sethostname", misc::sethostname(&p.hostname))?;

    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let group = fs::read_to_string("/etc/group").unwrap_or_default();
    let ids = resolve_user(&p.user, &passwd, &group).map_err(|e| format!("user: {e}"))?;
    for (name, soft, hard) in &p.ulimits {
        step(&format!("ulimit {name}"), misc::setrlimit(name, *soft, *hard))?;
    }

    step("keepcaps", misc::keep_caps(true))?;
    step("drop bounding set", p.caps.drop_bounding())?;
    step("set ids", misc::set_ids(ids.uid, ids.gid, &ids.groups))?;
    step("workdir", misc::chdir(&p.workdir).map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", p.workdir))))?;
    step("set capabilities", p.caps.set_current())?;
    step("no_new_privs", misc::no_new_privs())?;
    step("close descriptors", misc::close_range_cloexec(3))?;

    misc::reset_signal_state();
    let mut argv: Vec<*const libc::c_char> = p.argv.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> = p.envp.iter().map(|e| e.as_ptr()).collect();
    envp.push(std::ptr::null());
    unsafe { libc::execve(p.argv[0].as_ptr(), argv.as_ptr(), envp.as_ptr()) };
    Err(format!("exec: {}", std::io::Error::last_os_error()))
}
