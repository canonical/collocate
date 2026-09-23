use crate::env::{build_env, merge_env};
use crate::user::resolve_user;
use collocate_core::spec::Spec;
use collocate_core::{Error, Result};
use collocate_sys::caps::CapSet;
use collocate_sys::clone::{clone3, CloneFlags, Forked};
use collocate_sys::misc;
use std::ffi::CString;
use std::fs::{self, File};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

pub struct ExecRequest<'a> {
    pub init_pid: i32,
    pub init_pidfd: RawFd,
    pub cgroup_procs: PathBuf,
    pub spec: &'a Spec,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub user: Option<String>,
    pub workdir: Option<String>,
    pub stdio: [RawFd; 3],
}

struct Prepared {
    argv: Vec<CString>,
    envp: Vec<CString>,
    path_dirs: Vec<String>,
    user: String,
    workdir: String,
    caps: CapSet,
    init_pid: i32,
    init_pidfd: RawFd,
    cgroup_procs: PathBuf,
    stdio: [RawFd; 3],
}

const ALL_NS: i32 =
    libc::CLONE_NEWNS | libc::CLONE_NEWPID | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS | libc::CLONE_NEWNET | libc::CLONE_NEWCGROUP;

fn cstring(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::Invalid(format!("string contains NUL: {s:?}")))
}

pub fn spawn_exec(req: &ExecRequest) -> Result<(i32, OwnedFd)> {
    if req.argv.is_empty() {
        return Err(Error::Invalid("exec needs a command".into()));
    }
    let env = merge_env(build_env(req.spec), &req.env);
    let path_dirs =
        env.iter().find_map(|e| e.strip_prefix("PATH=")).unwrap_or("/usr/local/bin:/usr/bin:/bin").split(':').map(String::from).collect();
    let prepared = Prepared {
        argv: req.argv.iter().map(|a| cstring(a)).collect::<Result<_>>()?,
        envp: env.iter().map(|e| cstring(e)).collect::<Result<_>>()?,
        path_dirs,
        user: req.user.clone().unwrap_or_else(|| req.spec.process.user.clone()),
        workdir: req.workdir.clone().unwrap_or_else(|| req.spec.process.workdir.clone()),
        caps: CapSet::with_policy(&req.spec.caps.add, &req.spec.caps.drop)?,
        init_pid: req.init_pid,
        init_pidfd: req.init_pidfd,
        cgroup_procs: req.cgroup_procs.clone(),
        stdio: req.stdio,
    };
    match clone3(CloneFlags::empty(), None)? {
        collocate_sys::clone::Forked::Child => relay(&prepared),
        collocate_sys::clone::Forked::Parent { pid, pidfd } => Ok((pid, pidfd)),
    }
}

fn fail(p: &Prepared, code: i32, msg: &str) -> ! {
    let text = format!("exec: {msg}\n");
    unsafe {
        libc::write(p.stdio[2], text.as_ptr() as *const libc::c_void, text.len());
        libc::_exit(code)
    }
}

fn relay(p: &Prepared) -> ! {
    if let Err(e) = fs::write(&p.cgroup_procs, std::process::id().to_string()) {
        fail(p, 126, &format!("join cgroup: {e}"));
    }
    let root = match File::open(format!("/proc/{}/root", p.init_pid)) {
        Ok(f) => f,
        Err(e) => fail(p, 126, &format!("open root: {e}")),
    };
    if let Err(e) = misc::setns(p.init_pidfd, ALL_NS) {
        fail(p, 126, &format!("setns: {e}"));
    }
    if let Err(e) = misc::fchdir(root.as_raw_fd()).and_then(|_| misc::chroot_here()).and_then(|_| misc::chdir("/")) {
        fail(p, 126, &format!("enter root: {e}"));
    }
    match clone3(CloneFlags::empty(), None) {
        Ok(Forked::Child) => worker(p),
        Ok(Forked::Parent { pid, .. }) => match misc::waitpid(pid) {
            Ok(status) => unsafe { libc::_exit(misc::status_to_code(status)) },
            Err(e) => fail(p, 126, &format!("wait: {e}")),
        },
        Err(e) => fail(p, 126, &format!("fork: {e}")),
    }
}

fn worker(p: &Prepared) -> ! {
    for (i, fd) in p.stdio.iter().enumerate() {
        if let Err(e) = misc::dup2(*fd, i as i32) {
            fail(p, 126, &format!("stdio: {e}"));
        }
    }
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
    let group = fs::read_to_string("/etc/group").unwrap_or_default();
    let ids = match resolve_user(&p.user, &passwd, &group) {
        Ok(i) => i,
        Err(e) => fail(p, 126, &format!("user: {e}")),
    };
    let steps: [(&str, std::io::Result<()>); 1] = [("keepcaps", misc::keep_caps(true))];
    for (name, r) in steps {
        if let Err(e) = r {
            fail(p, 126, &format!("{name}: {e}"));
        }
    }
    if let Err(e) = p.caps.drop_bounding() {
        fail(p, 126, &format!("drop bounding set: {e}"));
    }
    if let Err(e) = misc::set_ids(ids.uid, ids.gid, &ids.groups) {
        fail(p, 126, &format!("set ids: {e}"));
    }
    if let Err(e) = misc::chdir(&p.workdir) {
        fail(p, 126, &format!("workdir {}: {e}", p.workdir));
    }
    if let Err(e) = p.caps.set_current().and_then(|_| misc::no_new_privs()) {
        fail(p, 126, &format!("caps: {e}"));
    }
    let _ = misc::close_range_cloexec(3);
    misc::reset_signal_state();

    let mut argv: Vec<*const libc::c_char> = p.argv.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    let mut envp: Vec<*const libc::c_char> = p.envp.iter().map(|e| e.as_ptr()).collect();
    envp.push(std::ptr::null());
    let name = p.argv[0].to_string_lossy().into_owned();
    if name.contains('/') {
        unsafe { libc::execve(p.argv[0].as_ptr(), argv.as_ptr(), envp.as_ptr()) };
    } else {
        for dir in &p.path_dirs {
            if let Ok(candidate) = CString::new(format!("{dir}/{name}")) {
                unsafe { libc::execve(candidate.as_ptr(), argv.as_ptr(), envp.as_ptr()) };
            }
        }
    }
    fail(p, 127, &format!("cannot execute {name}"))
}
