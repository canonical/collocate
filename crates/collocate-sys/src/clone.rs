use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloneFlags(u64);

impl CloneFlags {
    pub const NEWNS: CloneFlags = CloneFlags(0x0002_0000);
    pub const NEWCGROUP: CloneFlags = CloneFlags(0x0200_0000);
    pub const NEWUTS: CloneFlags = CloneFlags(0x0400_0000);
    pub const NEWIPC: CloneFlags = CloneFlags(0x0800_0000);
    pub const NEWUSER: CloneFlags = CloneFlags(0x1000_0000);
    pub const NEWPID: CloneFlags = CloneFlags(0x2000_0000);
    pub const NEWNET: CloneFlags = CloneFlags(0x4000_0000);

    pub fn empty() -> CloneFlags {
        CloneFlags(0)
    }

    pub fn bits(&self) -> u64 {
        self.0
    }
}

impl std::ops::BitOr for CloneFlags {
    type Output = CloneFlags;
    fn bitor(self, rhs: CloneFlags) -> CloneFlags {
        CloneFlags(self.0 | rhs.0)
    }
}

const CLONE_PIDFD: u64 = 0x0000_1000;
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

pub enum Forked {
    Child,
    Parent { pid: i32, pidfd: OwnedFd },
}

pub fn clone3(flags: CloneFlags, cgroup: Option<&File>) -> io::Result<Forked> {
    let mut pidfd: i32 = -1;
    let mut args = CloneArgs {
        flags: flags.bits() | CLONE_PIDFD,
        pidfd: &mut pidfd as *mut i32 as u64,
        exit_signal: libc::SIGCHLD as u64,
        ..CloneArgs::default()
    };
    if let Some(f) = cgroup {
        args.flags |= CLONE_INTO_CGROUP;
        args.cgroup = f.as_raw_fd() as u64;
    }
    let rc = unsafe { libc::syscall(libc::SYS_clone3, &mut args as *mut CloneArgs, std::mem::size_of::<CloneArgs>()) };
    match rc {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(Forked::Child),
        pid => Ok(Forked::Parent { pid: pid as i32, pidfd: unsafe { OwnedFd::from_raw_fd(pidfd) } }),
    }
}
