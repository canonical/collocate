use std::ffi::CString;
use std::io;

fn check(rc: i32) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn unshare(flags: u64) -> io::Result<()> {
    check(unsafe { libc::unshare(flags as i32) })
}

pub fn setns(fd: i32, flags: i32) -> io::Result<()> {
    check(unsafe { libc::setns(fd, flags) })
}

pub fn sethostname(name: &str) -> io::Result<()> {
    check(unsafe { libc::sethostname(name.as_ptr() as *const libc::c_char, name.len()) })
}

pub fn no_new_privs() -> io::Result<()> {
    check(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })
}

pub fn keep_caps(on: bool) -> io::Result<()> {
    check(unsafe { libc::prctl(libc::PR_SET_KEEPCAPS, i32::from(on), 0, 0, 0) })
}

pub fn close_range_cloexec(from: u32) -> io::Result<()> {
    let rc = unsafe { libc::syscall(libc::SYS_close_range, from, u32::MAX, 4u32) };
    check(rc as i32)
}

pub fn set_ids(uid: u32, gid: u32, groups: &[u32]) -> io::Result<()> {
    check(unsafe { libc::setgroups(groups.len(), groups.as_ptr()) })?;
    check(unsafe { libc::setgid(gid) })?;
    check(unsafe { libc::setuid(uid) })
}

pub fn setrlimit(name: &str, soft: u64, hard: u64) -> io::Result<()> {
    let resource = match name.to_ascii_lowercase().as_str() {
        "nofile" => libc::RLIMIT_NOFILE,
        "nproc" => libc::RLIMIT_NPROC,
        "memlock" => libc::RLIMIT_MEMLOCK,
        "core" => libc::RLIMIT_CORE,
        "stack" => libc::RLIMIT_STACK,
        "cpu" => libc::RLIMIT_CPU,
        "fsize" => libc::RLIMIT_FSIZE,
        "as" => libc::RLIMIT_AS,
        _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
    };
    let lim = libc::rlimit { rlim_cur: soft, rlim_max: hard };
    check(unsafe { libc::setrlimit(resource, &lim) })
}

pub fn chdir(path: &str) -> io::Result<()> {
    let p = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    check(unsafe { libc::chdir(p.as_ptr()) })
}

pub fn geteuid() -> u32 {
    unsafe { libc::geteuid() }
}

pub fn dup2(old: i32, new: i32) -> io::Result<()> {
    check(unsafe { libc::dup2(old, new) })
}

pub fn chown(path: &str, uid: u32, gid: u32) -> io::Result<()> {
    let p = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    check(unsafe { libc::chown(p.as_ptr(), uid, gid) })
}

pub fn chroot_here() -> io::Result<()> {
    check(unsafe { libc::chroot(c".".as_ptr()) })
}

pub fn fchdir(fd: i32) -> io::Result<()> {
    check(unsafe { libc::fchdir(fd) })
}

pub fn waitpid(pid: i32) -> io::Result<i32> {
    let mut status = 0;
    loop {
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc >= 0 {
            return Ok(status);
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    }
}

pub fn status_to_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    }
}

pub fn reset_signal_state() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
        for sig in 1..=31 {
            if sig != libc::SIGKILL && sig != libc::SIGSTOP {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
    }
}
