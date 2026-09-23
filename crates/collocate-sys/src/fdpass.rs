use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub fn socketpair_seqpacket() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))) }
}

pub fn send_fd(sock: &impl AsRawFd, data: &[u8], fds: &[&dyn AsRawFd]) -> io::Result<()> {
    let mut iov = libc::iovec { iov_base: data.as_ptr() as *mut libc::c_void, iov_len: data.len() };
    let raw: Vec<i32> = fds.iter().map(|f| f.as_raw_fd()).collect();
    let payload = std::mem::size_of_val(raw.as_slice()) as u32;
    let space = unsafe { libc::CMSG_SPACE(payload) } as usize;
    let mut control = vec![0u8; if raw.is_empty() { 0 } else { space }];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !raw.is_empty() {
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = space as _;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(payload) as _;
            std::ptr::copy_nonoverlapping(raw.as_ptr() as *const u8, libc::CMSG_DATA(cmsg), payload as usize);
        }
    }
    let rc = unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn recv_fd(sock: &impl AsRawFd, max_data: usize, max_fds: usize) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut data = vec![0u8; max_data];
    let mut iov = libc::iovec { iov_base: data.as_mut_ptr() as *mut libc::c_void, iov_len: data.len() };
    let space = unsafe { libc::CMSG_SPACE((max_fds * std::mem::size_of::<i32>()) as u32) } as usize;
    let mut control = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = space as _;
    let n = loop {
        let rc = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if rc >= 0 {
            break rc as usize;
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINTR) {
            return Err(e);
        }
    };
    data.truncate(n);
    let mut fds = Vec::new();
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let len = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let count = len / std::mem::size_of::<i32>();
                let base = libc::CMSG_DATA(cmsg) as *const i32;
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(base.add(i))));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    Ok((data, fds))
}

pub trait SendWithFds {
    fn send_with_fds(&mut self, req: &collocate_core::request::Request, fds: &[&dyn AsRawFd]) -> collocate_core::Result<()>;
}

impl SendWithFds for collocate_core::client::Client<std::os::unix::net::UnixStream> {
    fn send_with_fds(&mut self, req: &collocate_core::request::Request, fds: &[&dyn AsRawFd]) -> collocate_core::Result<()> {
        let payload = serde_json_bytes(req)?;
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&payload);
        send_fd(self.stream(), &frame, fds)?;
        Ok(())
    }
}

fn serde_json_bytes(req: &collocate_core::request::Request) -> collocate_core::Result<Vec<u8>> {
    let mut buf = Vec::new();
    collocate_core::wire::write_frame(&mut buf, req)?;
    Ok(buf[4..].to_vec())
}
