use collocate_sys::clone::{clone3, CloneFlags, Forked};
use collocate_sys::pidfd::{send_signal, wait, ExitStatus};
use std::fs;

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn wait_blocking(pidfd: &std::os::fd::OwnedFd) -> ExitStatus {
    loop {
        if let Some(s) = wait(pidfd, false).unwrap() {
            return s;
        }
    }
}

#[test]
fn clone3_without_namespaces_behaves_like_fork() {
    match clone3(CloneFlags::empty(), None).unwrap() {
        Forked::Child => unsafe { libc::_exit(7) },
        Forked::Parent { pidfd, .. } => assert_eq!(wait_blocking(&pidfd), ExitStatus::Code(7)),
    }
}

#[test]
fn wait_nohang_reports_running_children() {
    match clone3(CloneFlags::empty(), None).unwrap() {
        Forked::Child => unsafe {
            libc::pause();
            libc::_exit(0)
        },
        Forked::Parent { pidfd, .. } => {
            assert_eq!(wait(&pidfd, true).unwrap(), None);
            send_signal(&pidfd, libc::SIGTERM).unwrap();
            assert_eq!(wait_blocking(&pidfd), ExitStatus::Signal(libc::SIGTERM));
        }
    }
}

#[test]
fn new_pid_namespace_makes_the_child_pid_one() {
    if !is_root() {
        return;
    }
    let flags = CloneFlags::NEWNS | CloneFlags::NEWPID | CloneFlags::NEWIPC | CloneFlags::NEWUTS | CloneFlags::NEWNET;
    match clone3(flags, None).unwrap() {
        Forked::Child => unsafe { libc::_exit(if libc::getpid() == 1 { 0 } else { 1 }) },
        Forked::Parent { pidfd, .. } => assert_eq!(wait_blocking(&pidfd), ExitStatus::Code(0)),
    }
}

#[test]
fn child_can_be_born_inside_a_cgroup() {
    if !is_root() {
        return;
    }
    let dir = format!("/sys/fs/cgroup/collocate-sys-test-{}", std::process::id());
    fs::create_dir(&dir).unwrap();
    let fd = std::fs::File::open(&dir).unwrap();
    let result = match clone3(CloneFlags::empty(), Some(&fd)).unwrap() {
        Forked::Child => {
            let text = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
            unsafe { libc::_exit(if text.contains("collocate-sys-test") { 0 } else { 1 }) }
        }
        Forked::Parent { pidfd, .. } => wait_blocking(&pidfd),
    };
    let _ = fs::remove_dir(&dir);
    assert_eq!(result, ExitStatus::Code(0));
}
