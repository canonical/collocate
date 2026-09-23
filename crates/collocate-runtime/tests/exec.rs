mod common;
use collocate_runtime::exec::{spawn_exec, ExecRequest};
use collocate_runtime::launch::spawn;
use collocate_sys::pidfd::{send_signal, wait, ExitStatus};
use common::*;
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

fn exec_in(
    world: &World,
    spec: &collocate_core::spec::Spec,
    started_pid: i32,
    pidfd: i32,
    cg: &std::path::Path,
    argv: &[&str],
    user: Option<&str>,
) -> (ExitStatus, String) {
    let out_path = world.path("exec.out");
    let out = File::create(&out_path).unwrap();
    let null = File::open("/dev/null").unwrap();
    let req = ExecRequest {
        init_pid: started_pid,
        init_pidfd: pidfd,
        cgroup_procs: cg.join("cgroup.procs"),
        spec,
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        user: user.map(String::from),
        workdir: None,
        stdio: [null.as_raw_fd(), out.as_raw_fd(), out.as_raw_fd()],
    };
    let (_pid, efd) = spawn_exec(&req).unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = wait(&efd, true).unwrap() {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    };
    (status, fs::read_to_string(out_path).unwrap())
}

fn with_container(script: &str, f: impl FnOnce(&World, &collocate_core::spec::Spec, i32, i32, &std::path::Path)) {
    if !is_root() {
        return;
    }
    let _g = serial();
    let world = World::new();
    let cg = CgroupSandbox::new("exec");
    let mut spec = world.spec("boxed", script);
    spec.hostname = "boxed".into();
    let req = world.request(&spec, &cg.0);
    let started = spawn(&req).unwrap();
    started.confirm(Duration::from_secs(5)).unwrap();
    f(&world, &spec, started.pid, started.pidfd.as_raw_fd(), &cg.0);
    let _ = send_signal(&started.pidfd, libc::SIGKILL);
}

#[test]
fn exec_runs_inside_the_container_namespaces() {
    with_container("sleep 30", |w, s, pid, fd, cg| {
        let (st, out) = exec_in(w, s, pid, fd, cg, &["/bin/sh", "-c", "cat /proc/1/comm; hostname; cat /proc/self/cgroup"], None);
        assert_eq!(st, ExitStatus::Code(0), "{out}");
        assert_eq!(out.lines().collect::<Vec<_>>(), vec!["init", "boxed", "0::/"]);
    });
}

#[test]
fn exec_propagates_exit_codes_and_missing_commands() {
    with_container("sleep 30", |w, s, pid, fd, cg| {
        assert_eq!(exec_in(w, s, pid, fd, cg, &["/bin/sh", "-c", "exit 5"], None).0, ExitStatus::Code(5));
        assert_eq!(exec_in(w, s, pid, fd, cg, &["/no/such/binary"], None).0, ExitStatus::Code(127));
    });
}

#[test]
fn exec_can_switch_user_and_finds_commands_on_path() {
    with_container("sleep 30", |w, s, pid, fd, cg| {
        let (st, out) = exec_in(w, s, pid, fd, cg, &["id", "-u"], Some("nobody"));
        assert_eq!(st, ExitStatus::Code(0), "{out}");
        assert_eq!(out.trim(), "65534");
    });
}

#[test]
fn exec_processes_belong_to_the_container_cgroup() {
    with_container("sleep 30", |w, s, pid, fd, cg| {
        let (_, out) = exec_in(w, s, pid, fd, cg, &["/bin/sh", "-c", "cat /sys/fs/cgroup/pids.current"], None);
        let n: u32 = out.trim().parse().unwrap();
        assert!(n >= 3, "expected init, sleeping workload and the exec'd shell, got {n}");
    });
}
