use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub struct SignalFd {
    fd: OwnedFd,
}

impl SignalFd {
    pub fn new(signals: &[i32]) -> io::Result<SignalFd> {
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut set);
            for s in signals {
                libc::sigaddset(&mut set, *s);
            }
            if libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let fd = unsafe { libc::signalfd(-1, &set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SignalFd { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    pub fn read(&self) -> io::Result<Option<i32>> {
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        let rc = unsafe { libc::read(self.fd.as_raw_fd(), &mut info as *mut _ as *mut libc::c_void, size) };
        if rc == size as isize {
            return Ok(Some(info.ssi_signo as i32));
        }
        let e = io::Error::last_os_error();
        if rc < 0 && e.kind() == io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(e)
        }
    }
}

impl AsRawFd for SignalFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
