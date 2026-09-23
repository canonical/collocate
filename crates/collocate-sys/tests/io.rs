use collocate_sys::epoll::{Epoll, Events};
use collocate_sys::fdpass::{recv_fd, send_fd, socketpair_seqpacket};
use collocate_sys::timer::Timer;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

#[test]
fn epoll_reports_readiness_with_tokens() {
    let ep = Epoll::new().unwrap();
    let (r, w) = pipe();
    ep.add(r.as_raw_fd(), 42, Events::READABLE).unwrap();
    assert!(ep.wait(Some(Duration::from_millis(10))).unwrap().is_empty());
    let mut wf = std::fs::File::from(w);
    wf.write_all(b"x").unwrap();
    let ready = ep.wait(Some(Duration::from_millis(500))).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].token, 42);
    assert!(ready[0].readable);
}

#[test]
fn epoll_modify_and_remove() {
    let ep = Epoll::new().unwrap();
    let (r, w) = pipe();
    ep.add(r.as_raw_fd(), 1, Events::READABLE).unwrap();
    ep.remove(r.as_raw_fd()).unwrap();
    let mut wf = std::fs::File::from(w);
    wf.write_all(b"x").unwrap();
    assert!(ep.wait(Some(Duration::from_millis(20))).unwrap().is_empty());
}

#[test]
fn hangup_is_reported() {
    let ep = Epoll::new().unwrap();
    let (r, w) = pipe();
    ep.add(r.as_raw_fd(), 9, Events::READABLE).unwrap();
    drop(w);
    let ready = ep.wait(Some(Duration::from_millis(200))).unwrap();
    assert!(ready[0].hangup);
}

#[test]
fn timers_fire_once_and_can_be_rearmed() {
    let t = Timer::new().unwrap();
    let ep = Epoll::new().unwrap();
    ep.add(t.as_raw_fd(), 5, Events::READABLE).unwrap();
    t.arm(Duration::from_millis(20)).unwrap();
    assert!(ep.wait(Some(Duration::from_millis(5))).unwrap().is_empty());
    let ready = ep.wait(Some(Duration::from_millis(500))).unwrap();
    assert_eq!(ready[0].token, 5);
    assert_eq!(t.consume().unwrap(), 1);
    assert!(ep.wait(Some(Duration::from_millis(50))).unwrap().is_empty());
    t.arm(Duration::from_millis(10)).unwrap();
    assert_eq!(ep.wait(Some(Duration::from_millis(500))).unwrap().len(), 1);
}

#[test]
fn descriptors_travel_over_seqpacket_sockets() {
    let (a, b) = socketpair_seqpacket().unwrap();
    let (r, w) = pipe();
    send_fd(&a, b"hello", &[&w]).unwrap();
    let (data, fds) = recv_fd(&b, 64, 4).unwrap();
    assert_eq!(data, b"hello");
    assert_eq!(fds.len(), 1);
    let mut wf = std::fs::File::from(fds.into_iter().next().unwrap());
    wf.write_all(b"ping").unwrap();
    drop(wf);
    drop(w);
    let mut buf = String::new();
    std::fs::File::from(r).read_to_string(&mut buf).unwrap();
    assert_eq!(buf, "ping");
}

#[test]
fn plain_messages_preserve_boundaries() {
    let (a, b) = socketpair_seqpacket().unwrap();
    send_fd(&a, b"one", &[]).unwrap();
    send_fd(&a, b"two", &[]).unwrap();
    assert_eq!(recv_fd(&b, 64, 1).unwrap().0, b"one");
    assert_eq!(recv_fd(&b, 64, 1).unwrap().0, b"two");
    drop(a);
    assert!(recv_fd(&b, 64, 1).unwrap().0.is_empty());
}

#[test]
fn any_socket_type_can_carry_descriptors() {
    use std::os::unix::net::UnixStream;
    let (a, b) = UnixStream::pair().unwrap();
    let (r, w) = pipe();
    send_fd(&a, b"stream", &[&w]).unwrap();
    let (data, fds) = recv_fd(&b, 64, 2).unwrap();
    assert_eq!(data, b"stream");
    assert_eq!(fds.len(), 1);
    drop(r);
}
