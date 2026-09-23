use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

pub struct Timer {
    fd: OwnedFd,
}

fn spec(initial: Duration, interval: Duration) -> libc::itimerspec {
    let ts = |d: Duration| libc::timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ };
    libc::itimerspec { it_interval: ts(interval), it_value: ts(initial) }
}

impl Timer {
    pub fn new() -> io::Result<Timer> {
        let fd = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Timer { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    fn set(&self, s: libc::itimerspec) -> io::Result<()> {
        let rc = unsafe { libc::timerfd_settime(self.fd.as_raw_fd(), 0, &s, std::ptr::null_mut()) };
        if rc != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn arm(&self, after: Duration) -> io::Result<()> {
        let after = if after.is_zero() { Duration::from_nanos(1) } else { after };
        self.set(spec(after, Duration::ZERO))
    }

    pub fn arm_interval(&self, every: Duration) -> io::Result<()> {
        self.set(spec(every, every))
    }

    pub fn disarm(&self) -> io::Result<()> {
        self.set(spec(Duration::ZERO, Duration::ZERO))
    }

    pub fn consume(&self) -> io::Result<u64> {
        let mut buf = [0u8; 8];
        let rc = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr() as *mut libc::c_void, 8) };
        if rc == 8 {
            Ok(u64::from_ne_bytes(buf))
        } else if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock {
            Ok(0)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl AsRawFd for Timer {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
