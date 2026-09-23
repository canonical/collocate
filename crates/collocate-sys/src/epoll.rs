use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Events(u32);

impl Events {
    pub const READABLE: Events = Events(libc::EPOLLIN as u32);
    pub const WRITABLE: Events = Events(libc::EPOLLOUT as u32);
}

impl std::ops::BitOr for Events {
    type Output = Events;
    fn bitor(self, rhs: Events) -> Events {
        Events(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub token: u64,
    pub readable: bool,
    pub writable: bool,
    pub hangup: bool,
    pub error: bool,
}

pub struct Epoll {
    fd: OwnedFd,
}

impl Epoll {
    pub fn new() -> io::Result<Epoll> {
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Epoll { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    fn ctl(&self, op: i32, fd: RawFd, token: u64, events: Events) -> io::Result<()> {
        let mut ev = libc::epoll_event { events: events.0, u64: token };
        let rc = unsafe { libc::epoll_ctl(self.fd.as_raw_fd(), op, fd, &mut ev) };
        if rc != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn add(&self, fd: RawFd, token: u64, events: Events) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_ADD, fd, token, events)
    }

    pub fn modify(&self, fd: RawFd, token: u64, events: Events) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_MOD, fd, token, events)
    }

    pub fn remove(&self, fd: RawFd) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_DEL, fd, 0, Events(0))
    }

    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Vec<Ready>> {
        let mut buf = [libc::epoll_event { events: 0, u64: 0 }; 64];
        let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
        let n = unsafe { libc::epoll_wait(self.fd.as_raw_fd(), buf.as_mut_ptr(), buf.len() as i32, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(libc::EINTR) { Ok(Vec::new()) } else { Err(e) };
        }
        Ok(buf[..n as usize]
            .iter()
            .map(|e| {
                let bits = e.events;
                Ready {
                    token: e.u64,
                    readable: bits & libc::EPOLLIN as u32 != 0,
                    writable: bits & libc::EPOLLOUT as u32 != 0,
                    hangup: bits & (libc::EPOLLHUP | libc::EPOLLRDHUP) as u32 != 0,
                    error: bits & libc::EPOLLERR as u32 != 0,
                }
            })
            .collect())
    }
}
