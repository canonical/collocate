use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
}

impl ExitStatus {
    pub fn as_code(&self) -> i32 {
        match self {
            ExitStatus::Code(c) => *c,
            ExitStatus::Signal(s) => 128 + s,
        }
    }
}

pub fn pidfd_open(pid: i32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
    }
}

pub fn send_signal(pidfd: &OwnedFd, signal: i32) -> io::Result<()> {
    let rc = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, pidfd.as_raw_fd(), signal, 0, 0) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn wait(pidfd: &OwnedFd, nohang: bool) -> io::Result<Option<ExitStatus>> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let options = libc::WEXITED | if nohang { libc::WNOHANG } else { 0 };
        let rc = unsafe { libc::waitid(libc::P_PIDFD, pidfd.as_raw_fd() as libc::id_t, &mut info, options) };
        if rc != 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        let pid = unsafe { info.si_pid() };
        if pid == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        return Ok(Some(match info.si_code {
            libc::CLD_EXITED => ExitStatus::Code(status),
            _ => ExitStatus::Signal(status),
        }));
    }
}
