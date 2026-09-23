use crate::clone::{clone3, CloneFlags, Forked};
use crate::pidfd::{pidfd_open, wait};
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn check(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }

    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    fn push(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        self.checks.push(Check { name: name.to_string(), ok, detail: detail.into() });
    }
}

const CGROUP2_MAGIC: i64 = 0x6367_7270;

fn cgroup2_mounted() -> bool {
    let path = std::ffi::CString::new("/sys/fs/cgroup").unwrap_or_default();
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    unsafe { libc::statfs(path.as_ptr(), &mut st) == 0 && st.f_type as i64 == CGROUP2_MAGIC }
}

pub fn probe() -> Report {
    let mut r = Report::default();
    let cg = cgroup2_mounted();
    r.push("cgroup2", cg, if cg { "unified hierarchy mounted at /sys/fs/cgroup" } else { "cgroup v2 is not mounted at /sys/fs/cgroup" });

    let controllers = fs::read_to_string("/sys/fs/cgroup/cgroup.controllers").unwrap_or_default();
    let missing: Vec<&str> = ["cpu", "memory", "pids"].into_iter().filter(|c| !controllers.split_whitespace().any(|x| x == *c)).collect();
    r.push(
        "cgroup-controllers",
        missing.is_empty(),
        if missing.is_empty() {
            "cpu, memory and pids are available".to_string()
        } else {
            format!("missing controllers: {}", missing.join(", "))
        },
    );

    let fs_list = fs::read_to_string("/proc/filesystems").unwrap_or_default();
    let overlay = fs_list.lines().any(|l| l.split_whitespace().last() == Some("overlay"));
    r.push("overlayfs", overlay, if overlay { "overlay is registered" } else { "overlay is not listed in /proc/filesystems" });

    let has_fuse_dev = fs::metadata("/dev/fuse").is_ok();
    let has_fuse_overlayfs = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| fs::metadata(format!("{d}/fuse-overlayfs")).is_ok_and(|m| m.is_file()));
    let fuse_ok = has_fuse_dev && has_fuse_overlayfs;
    r.push(
        "fuse-overlayfs",
        fuse_ok,
        match (has_fuse_dev, has_fuse_overlayfs) {
            (true, true) => "fuse-overlayfs and /dev/fuse are both available".to_string(),
            (false, _) => "/dev/fuse is not present".to_string(),
            (true, false) => "fuse-overlayfs is not on PATH".to_string(),
        },
    );

    let cloned = match clone3(CloneFlags::empty(), None) {
        Ok(Forked::Child) => unsafe { libc::_exit(0) },
        Ok(Forked::Parent { pidfd, .. }) => loop {
            match wait(&pidfd, false) {
                Ok(Some(_)) => break Ok(()),
                Ok(None) => continue,
                Err(e) => break Err(e),
            }
        },
        Err(e) => Err(e),
    };
    match cloned {
        Ok(()) => r.push("clone3", true, "clone3 with CLONE_PIDFD succeeded"),
        Err(e) => r.push("clone3", false, format!("clone3 failed: {e}")),
    }

    match pidfd_open(std::process::id() as i32) {
        Ok(_) => r.push("pidfd", true, "pidfd_open succeeded"),
        Err(e) => r.push("pidfd", false, format!("pidfd_open failed: {e}")),
    }
    r
}
