use collocate_sys::epoll::{Epoll, Events};
use collocate_sys::signals::SignalFd;
use std::os::fd::AsRawFd;
use std::time::Duration;

#[test]
fn blocked_signals_are_delivered_through_the_descriptor() {
    let sfd = SignalFd::new(&[libc::SIGUSR1, libc::SIGUSR2]).unwrap();
    assert_eq!(sfd.read().unwrap(), None);
    unsafe { libc::raise(libc::SIGUSR2) };
    let ep = Epoll::new().unwrap();
    ep.add(sfd.as_raw_fd(), 1, Events::READABLE).unwrap();
    assert_eq!(ep.wait(Some(Duration::from_millis(500))).unwrap().len(), 1);
    assert_eq!(sfd.read().unwrap(), Some(libc::SIGUSR2));
    assert_eq!(sfd.read().unwrap(), None);
}
