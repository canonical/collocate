mod common;
use collocate_core::limits::Limits;
use collocate_core::spec::Mount;
use collocate_runtime::launch::spawn;
use collocate_sys::caps::default_set;
use collocate_sys::pidfd::{send_signal, ExitStatus};
use common::*;
use std::fs;
use std::time::Duration;

macro_rules! need_root {
    () => {
        if !is_root() {
            return;
        }
        let _guard = serial();
    };
}

fn run(script: &str) -> (ExitStatus, String) {
    let world = World::new();
    let cg = CgroupSandbox::new("run");
    let spec = world.spec("t", script);
    run_to_completion(&world, &spec, &cg.0)
}

#[test]
fn workload_runs_under_init_in_its_own_pid_namespace() {
    need_root!();
    let (status, log) = run("echo pid=$$; cat /proc/1/comm");
    assert_eq!(status, ExitStatus::Code(0), "{log}");
    assert!(log.contains("pid=2"), "{log}");
    assert!(log.contains("init"), "{log}");
}

#[test]
fn exit_status_propagates() {
    need_root!();
    assert_eq!(run("exit 7").0, ExitStatus::Code(7));
}

#[test]
fn hostname_is_isolated() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("host");
    let mut spec = world.spec("t", "hostname");
    spec.hostname = "boxed".into();
    let before = fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
    let (_, log) = run_to_completion(&world, &spec, &cg.0);
    assert_eq!(log.trim(), "boxed");
    assert_eq!(fs::read_to_string("/proc/sys/kernel/hostname").unwrap(), before);
}

#[test]
fn root_is_read_only_while_declared_tmpfs_is_writable() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("ro");
    let mut spec = world.spec("t", "touch /x 2>/dev/null; echo rc=$?; touch /tmp/x; echo rc2=$?");
    spec.mounts.push(Mount::Tmpfs { dst: "/tmp".into(), size: Some(1 << 20) });
    let (_, log) = run_to_completion(&world, &spec, &cg.0);
    assert!(log.contains("rc=1"), "{log}");
    assert!(log.contains("rc2=0"), "{log}");
    assert!(!world.rootfs.join("x").exists());
}

#[test]
fn capabilities_and_no_new_privs_are_applied() {
    need_root!();
    let (status, log) = run("grep -E 'CapBnd|NoNewPrivs' /proc/self/status");
    assert_eq!(status, ExitStatus::Code(0), "{log}");
    let (lo, hi) = default_set().words();
    let want = format!("{:016x}", (u64::from(hi) << 32) | u64::from(lo));
    assert!(log.contains(&want), "expected {want} in {log}");
    assert!(log.contains("NoNewPrivs:\t1"), "{log}");
}

#[test]
fn cgroup_namespace_and_limits_are_visible_inside() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("lim");
    let dir = cg.0.join("c");
    fs::create_dir(&dir).unwrap();
    fs::write(cg.0.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
    let limits = Limits { memory: Some(64 << 20), pids_max: 100, ..Limits::default() };
    for (k, v) in limits.cgroup_writes() {
        fs::write(dir.join(&k), v).unwrap();
    }
    let spec = world.spec("t", "cat /proc/self/cgroup; cat /sys/fs/cgroup/memory.max; cat /sys/fs/cgroup/pids.max");
    let (status, log) = run_to_completion(&world, &spec, &dir);
    assert_eq!(status, ExitStatus::Code(0), "{log}");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines, vec!["0::/", "67108864", "100"], "{log}");
}

#[test]
fn sensitive_proc_paths_are_masked() {
    need_root!();
    let (_, log) =
        run("grep -c ' /proc/kcore ' /proc/self/mountinfo; grep -c ' /proc/sysrq-trigger ' /proc/self/mountinfo; wc -c < /proc/keys");
    let out: Vec<&str> = log.split_whitespace().collect();
    assert_eq!(out, vec!["1", "1", "0"], "{log}");
}

#[test]
fn generated_files_are_mounted() {
    need_root!();
    let (_, log) = run("cat /etc/resolv.conf /etc/hosts /etc/hostname");
    assert!(log.contains("nameserver 10.1.2.3"), "{log}");
    assert!(log.contains("10.9.9.9\tpeer"), "{log}");
    assert!(log.contains("t\n"), "{log}");
}

#[test]
fn environment_reaches_the_workload() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("env");
    let mut spec = world.spec("t", "env");
    spec.process.env.push(("FOO".into(), "bar".into()));
    let (_, log) = run_to_completion(&world, &spec, &cg.0);
    assert!(log.contains("FOO=bar"), "{log}");
    assert!(log.contains("HOSTNAME=t"), "{log}");
}

#[test]
fn secrets_are_read_only_and_volumes_are_writable() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("mnt");
    let mut spec = world
        .spec("t", "cat /run/secrets/pw; echo x > /run/secrets/pw 2>/dev/null; echo secretrc=$?; echo data > /data/out; echo volrc=$?");
    spec.mounts.push(Mount::Secret { name: "pw".into(), dst: "/run/secrets/pw".into() });
    let host_data = world.path("hostdata");
    fs::create_dir_all(&host_data).unwrap();
    spec.mounts.push(Mount::Bind { src: host_data.to_string_lossy().into(), dst: "/data".into(), ro: false });
    let req = world.request(&spec, &cg.0);
    fs::write(req.extras.secrets_dir.join("pw"), "hunter2\n").unwrap();
    let started = spawn(&req).unwrap();
    started.confirm(Duration::from_secs(5)).unwrap();
    wait_exit(&started);
    let log = fs::read_to_string(&req.log_path).unwrap();
    assert!(log.contains("hunter2"), "{log}");
    assert!(!log.contains("secretrc=0"), "{log}");
    assert!(log.contains("volrc=0"), "{log}");
    assert_eq!(fs::read_to_string(host_data.join("out")).unwrap(), "data\n");
}

#[test]
fn users_are_resolved_inside_the_container() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("user");
    let mut spec = world.spec("t", "id -u; id -g");
    spec.process.user = "nobody".into();
    let (status, log) = run_to_completion(&world, &spec, &cg.0);
    assert_eq!(status, ExitStatus::Code(0), "{log}");
    assert_eq!(log.split_whitespace().collect::<Vec<_>>(), vec!["65534", "65534"]);
}

#[test]
fn setup_failures_are_reported_to_the_launcher() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("fail");
    let mut spec = world.spec("t", "true");
    spec.process.workdir = "/does/not/exist".into();
    let req = world.request(&spec, &cg.0);
    let started = spawn(&req).unwrap();
    let err = started.confirm(Duration::from_secs(5)).unwrap_err().to_string();
    assert!(err.contains("workdir"), "{err}");
}

#[test]
fn killing_the_cgroup_terminates_the_container() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("kill");
    let spec = world.spec("t", "sleep 60");
    let req = world.request(&spec, &cg.0);
    let started = spawn(&req).unwrap();
    started.confirm(Duration::from_secs(5)).unwrap();
    send_signal(&started.pidfd, libc::SIGKILL).unwrap();
    assert_eq!(wait_exit(&started), ExitStatus::Signal(libc::SIGKILL));
}

#[test]
fn the_host_never_sees_container_mounts() {
    need_root!();
    let world = World::new();
    let cg = CgroupSandbox::new("leak");
    let spec = world.spec("t", "true");
    let before = fs::read_to_string("/proc/self/mountinfo").unwrap().lines().count();
    run_to_completion(&world, &spec, &cg.0);
    assert_eq!(fs::read_to_string("/proc/self/mountinfo").unwrap().lines().count(), before);
}

#[test]
fn workloads_start_with_a_clean_signal_state() {
    need_root!();
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let (status, log) = run("grep -E 'SigBlk|SigIgn' /proc/self/status");
    assert_eq!(status, ExitStatus::Code(0), "{log}");
    assert!(log.contains("SigBlk:\t0000000000000000"), "{log}");
    let ign = log.lines().find_map(|l| l.strip_prefix("SigIgn:\t")).and_then(|h| u64::from_str_radix(h.trim(), 16).ok()).unwrap();
    assert_eq!(ign & (1 << (libc::SIGPIPE - 1)), 0, "SIGPIPE must not be ignored: {log}");
    assert_eq!(ign & (1 << (libc::SIGTERM - 1)), 0, "{log}");
}
