use collocate_core::client::Client;
use collocate_core::request::{HealthState, Request, Response, State};
use collocate_core::spec::{HealthKind, Mount, RootSource, Series, Spec};
use collocate_core::Error;
use collocate_sys::fdpass::SendWithFds;
use collocated::config::{Config, RootModeSetting};
use collocated::daemon::Daemon;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn init_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = root.join("target/x86_64-unknown-linux-musl/debug/collocate-init");
    if !bin.exists() {
        assert!(std::process::Command::new("cargo").args(["build", "-p", "collocate-init"]).current_dir(&root).status().unwrap().success());
    }
    bin
}

struct Env {
    dir: tempfile::TempDir,
    sock: PathBuf,
    cg_root: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}

const APPLETS: [&str; 16] =
    ["sh", "cat", "ls", "echo", "grep", "id", "hostname", "touch", "wc", "sleep", "env", "true", "false", "head", "nc", "test"];

fn build_image(state: &Path) {
    let rootfs = state.join("images/noble/test-build/rootfs");
    for d in ["bin", "etc", "proc", "sys", "dev", "run", "tmp", "data", "root", ".collocate"] {
        fs::create_dir_all(rootfs.join(d)).unwrap();
    }
    for f in ["etc/resolv.conf", "etc/hosts", "etc/hostname", ".collocate/init"] {
        fs::write(rootfs.join(f), "").unwrap();
    }
    fs::write(rootfs.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();
    fs::write(rootfs.join("etc/group"), "root:x:0:\n").unwrap();
    fs::copy("/usr/bin/busybox", rootfs.join("bin/busybox")).unwrap();
    for a in APPLETS {
        symlink("busybox", rootfs.join("bin").join(a)).unwrap();
    }
    symlink("test-build", state.join("images/noble/latest")).unwrap();
}

fn build_two_layer_oci_image(state: &Path) -> Vec<String> {
    let bottom = "sha256:bottomlayer";
    let top = "sha256:toplayer";
    let bdir = state.join("layers").join(bottom.replace(':', "-"));
    for d in ["bin", "etc", "proc", "sys", "dev", "run", "tmp", "root", ".collocate"] {
        fs::create_dir_all(bdir.join(d)).unwrap();
    }
    for f in ["etc/resolv.conf", "etc/hosts", "etc/hostname", ".collocate/init"] {
        fs::write(bdir.join(f), "").unwrap();
    }
    fs::write(bdir.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();
    fs::write(bdir.join("etc/group"), "root:x:0:\n").unwrap();
    fs::copy("/usr/bin/busybox", bdir.join("bin/busybox")).unwrap();
    for a in APPLETS {
        symlink("busybox", bdir.join("bin").join(a)).unwrap();
    }
    fs::write(bdir.join("from-bottom"), "bottom-layer\n").unwrap();

    let tdir = state.join("layers").join(top.replace(':', "-"));
    fs::create_dir_all(&tdir).unwrap();
    fs::write(tdir.join("from-top"), "top-layer\n").unwrap();

    vec![bottom.to_string(), top.to_string()]
}

impl Env {
    fn start() -> Env {
        Env::start_with(RootModeSetting::BindRo, |_| {})
    }

    fn start_with(root_mode: RootModeSetting, prep: impl FnOnce(&Path)) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let run = dir.path().join("run");
        fs::create_dir_all(&run).unwrap();
        build_image(&state);
        prep(&state);
        let cg_root = PathBuf::from(format!("/sys/fs/cgroup/collocate-e2e-{}", std::process::id()));
        fs::create_dir(&cg_root).unwrap();
        fs::write(cg_root.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
        let cfg = Config {
            state_dir: state,
            run_dir: run.clone(),
            root_mode,
            move_self_to_supervisor: false,
            init_path: init_binary(),
            cgroup_root: cg_root.clone(),
            bridge: format!("colt{}", std::process::id() % 10000),
            subnet: "10.213.0.0/24".into(),
            ..Config::default()
        };
        let sock = cfg.socket();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut d = Daemon::new(cfg).expect("daemon");
            tx.send(()).unwrap();
            d.run().expect("run");
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Env { dir, sock, cg_root, thread: Some(thread) }
    }

    fn client(&self) -> Client<UnixStream> {
        Client::connect(&self.sock).unwrap()
    }

    fn spec(&self, name: &str, script: &str) -> Spec {
        let mut s = Spec::new(
            name,
            RootSource::Base { series: Series::Noble, build_id: "latest".into() },
            vec!["/bin/sh".into(), "-c".into(), script.into()],
        );
        s.process.env.push(("PATH".into(), "/bin".into()));
        s
    }

    fn run(&self, spec: Spec) -> collocate_core::ContainerId {
        match self.client().call(&Request::Run(Box::new(spec))).unwrap() {
            Response::Id { id } => id,
            other => panic!("{other:?}"),
        }
    }

    fn wait_for(&self, target: &str) -> i32 {
        match self.client().call(&Request::Wait { target: target.into() }).unwrap() {
            Response::Exit { status } => status,
            other => panic!("{other:?}"),
        }
    }

    fn logs(&self, target: &str) -> String {
        match self.client().call(&Request::Logs { target: target.into(), tail: None, offset: None }).unwrap() {
            Response::Log { data, .. } => data,
            other => panic!("{other:?}"),
        }
    }

    fn ps(&self) -> Vec<collocate_core::request::ContainerInfo> {
        match self.client().call(&Request::Ps { all: true, project: None }).unwrap() {
            Response::Containers(c) => c,
            other => panic!("{other:?}"),
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Ok(mut c) = Client::connect(&self.sock) {
            let _ = c.call(&Request::Shutdown);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = fs::write(self.cg_root.join("cgroup.kill"), "1");
        std::thread::sleep(Duration::from_millis(100));
        for sub in ["collocate.slice/containers", "collocate.slice/supervisor", "collocate.slice"] {
            if let Ok(rd) = fs::read_dir(self.cg_root.join(sub)) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        let _ = fs::remove_dir(e.path());
                    }
                }
            }
            let _ = fs::remove_dir(self.cg_root.join(sub));
        }
        let _ = fs::remove_dir(&self.cg_root);
        let _ = std::process::Command::new("ip").args(["link", "del", &format!("colt{}", std::process::id() % 10000)]).output();
        let _ = self.dir.path();
    }
}

macro_rules! need_root {
    () => {
        if !is_root() {
            return;
        }
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    };
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn run_wait_and_logs() {
    need_root!();
    let env = Env::start();
    let id = env.run(env.spec("hello", "echo hello-from-container; exit 3"));
    assert_eq!(env.wait_for(&id.to_string()), 3);
    assert!(env.logs("hello").contains("hello-from-container"));
}

#[test]
fn exec_healthchecks_run_the_real_command_and_can_fail() {
    need_root!();
    let env = Env::start();
    let mut ok_spec = env.spec("hc-ok", "touch /tmp/ready; sleep 30");
    ok_spec.healthcheck = Some(collocate_core::spec::Healthcheck {
        kind: HealthKind::Exec { argv: vec!["/bin/sh".into(), "-c".into(), "test -f /tmp/ready".into()] },
        interval_secs: 1,
        timeout_secs: 2,
        retries: 1,
        start_period_secs: 0,
    });
    env.run(ok_spec);
    wait_until("healthy", || env.ps().iter().any(|c| c.name == "hc-ok" && c.health == Some(HealthState::Healthy)));

    let mut fail_spec = env.spec("hc-fail", "sleep 30");
    fail_spec.healthcheck = Some(collocate_core::spec::Healthcheck {
        kind: HealthKind::Exec { argv: vec!["/bin/sh".into(), "-c".into(), "exit 1".into()] },
        interval_secs: 1,
        timeout_secs: 2,
        retries: 1,
        start_period_secs: 0,
    });
    env.run(fail_spec);
    wait_until("unhealthy", || env.ps().iter().any(|c| c.name == "hc-fail" && c.health == Some(HealthState::Unhealthy)));
}

#[test]
fn fuse_overlayfs_stacks_layers_and_is_writable() {
    need_root!();
    let mut layers = Vec::new();
    let env = Env::start_with(RootModeSetting::FuseOverlay, |state| layers = build_two_layer_oci_image(state));
    let mut spec = Spec::new(
        "layered",
        RootSource::Oci { digest: "sha256:cfg".into(), layers },
        vec!["/bin/sh".into(), "-c".into(), "cat /from-bottom /from-top > /result; echo more >> /from-top; sleep 30".into()],
    );
    spec.process.env.push(("PATH".into(), "/bin".into()));
    let id = env.run(spec);
    let probe = |argv: &[&str]| -> i32 {
        match env
            .client()
            .call(&Request::ExecProbe { target: id.to_string(), argv: argv.iter().map(|s| s.to_string()).collect(), timeout_secs: 5 })
            .unwrap()
        {
            Response::Exit { status } => status,
            other => panic!("{other:?}"),
        }
    };
    wait_until("result written", || probe(&["test", "-f", "/result"]) == 0);
    assert_eq!(probe(&["grep", "-q", "bottom-layer", "/result"]), 0, "bottom layer content missing from the merge");
    assert_eq!(probe(&["grep", "-q", "top-layer", "/result"]), 0, "top layer content missing from the merge");
    assert_eq!(probe(&["grep", "-q", "more", "/from-top"]), 0, "write into the merge did not persist");
    assert_eq!(probe(&["test", "-w", "/"]), 0, "root should be writable under fuse-overlayfs");

    let merged = env.dir.path().join("run/fusemerge").join(id.to_string());
    assert!(merged.exists());
    env.client().call(&Request::Kill { target: id.to_string(), signal: 9 }).unwrap();
    env.wait_for(&id.to_string());
    wait_until("fuse mount torn down", || !merged.exists());
}

#[test]
fn commit_captures_writable_changes_into_a_new_image() {
    need_root!();
    let mut layers = Vec::new();
    let env = Env::start_with(RootModeSetting::FuseOverlay, |state| layers = build_two_layer_oci_image(state));
    let mut spec = Spec::new(
        "committable",
        RootSource::Oci { digest: "sha256:cfg".into(), layers: layers.clone() },
        vec!["/bin/sh".into(), "-c".into(), "echo committed > /new-file; sleep 30".into()],
    );
    spec.process.env.push(("PATH".into(), "/bin".into()));
    spec.persistent = true;
    let id = env.run(spec);
    let probe = |argv: &[&str]| -> i32 {
        match env
            .client()
            .call(&Request::ExecProbe { target: id.to_string(), argv: argv.iter().map(|s| s.to_string()).collect(), timeout_secs: 5 })
            .unwrap()
        {
            Response::Exit { status } => status,
            other => panic!("{other:?}"),
        }
    };
    wait_until("new file written", || probe(&["test", "-f", "/new-file"]) == 0);

    let digest = match env.client().call(&Request::Commit { target: id.to_string(), image: "snap:v1".into() }).unwrap() {
        Response::Text { text } => text,
        other => panic!("{other:?}"),
    };
    assert!(digest.starts_with("sha256:"), "{digest}");

    let store = collocate_image::config::ImageStore::new(env.dir.path().join("state"));
    let meta = store.get("snap:v1").unwrap();
    assert_eq!(meta.layers.len(), layers.len() + 1);

    let ov = collocate_image::config::RunOverrides {
        command: vec!["/bin/sh".into(), "-c".into(), "cat /new-file /from-bottom /from-top; sleep 30".into()],
        entrypoint: None,
        env: vec![],
        user: None,
        workdir: None,
        publish_exposed: false,
    };
    let mut from_commit = collocate_image::config::spec_from_image(&meta, &ov).unwrap();
    from_commit.name = "from-commit".into();
    from_commit.hostname = "from-commit".into();
    from_commit.process.env.push(("PATH".into(), "/bin".into()));
    env.run(from_commit);
    wait_until("committed container logged its output", || !env.logs("from-commit").is_empty());
    let log = env.logs("from-commit");
    assert!(log.contains("committed") && log.contains("bottom-layer") && log.contains("top-layer"), "{log}");
}

#[test]
fn commit_refuses_containers_started_from_the_base_image() {
    need_root!();
    let env = Env::start_with(RootModeSetting::FuseOverlay, |_| {});
    let id = env.run(env.spec("plain", "sleep 30"));
    let err = env.client().call(&Request::Commit { target: id.to_string(), image: "x".into() }).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

#[test]
fn commit_refuses_when_the_host_has_no_writable_layer() {
    need_root!();
    let mut layers = Vec::new();
    let env = Env::start_with(RootModeSetting::BindRo, |state| layers = build_two_layer_oci_image(state));
    let mut spec = Spec::new(
        "bindro",
        RootSource::Oci { digest: "sha256:cfg".into(), layers: vec![layers[0].clone()] },
        vec!["/bin/sleep".into(), "30".into()],
    );
    spec.process.env.push(("PATH".into(), "/bin".into()));
    let id = env.run(spec);
    let err = env.client().call(&Request::Commit { target: id.to_string(), image: "x".into() }).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
}

#[test]
fn ps_reports_state_and_addresses() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("sleeper", "sleep 30"));
    let list = env.ps();
    let c = list.iter().find(|c| c.name == "sleeper").unwrap();
    assert_eq!(c.state, State::Running);
    assert!(c.pid.is_some());
    assert!(c.address.unwrap().to_string().starts_with("10.213.0."));
}

#[test]
fn names_are_unique_and_lookups_accept_prefixes() {
    need_root!();
    let env = Env::start();
    let id = env.run(env.spec("dup", "sleep 30"));
    let err = env.client().call(&Request::Run(Box::new(env.spec("dup", "true")))).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
    let prefix = id.to_string()[..6].to_string();
    assert!(matches!(env.client().call(&Request::Kill { target: prefix, signal: 9 }).unwrap(), Response::Ok));
    assert!(matches!(env.client().call(&Request::Kill { target: "ghost".into(), signal: 9 }), Err(Error::NotFound(_))));
}

#[test]
fn stop_terminates_gracefully_and_kill_forces() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("graceful", "trap 'exit 0' TERM; while :; do sleep 0.1; done"));
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    assert!(matches!(env.client().call(&Request::Stop { target: "graceful".into(), timeout_secs: Some(5) }).unwrap(), Response::Ok));
    eprintln!("graceful stop took {:?}", start.elapsed());
    assert!(start.elapsed() < Duration::from_secs(4));

    env.run(env.spec("stubborn", "trap '' TERM; while :; do sleep 0.1; done"));
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    assert!(matches!(env.client().call(&Request::Stop { target: "stubborn".into(), timeout_secs: Some(1) }).unwrap(), Response::Ok));
    assert!(start.elapsed() >= Duration::from_millis(900) && start.elapsed() < Duration::from_secs(5));
    wait_until("stubborn to stop", || env.ps().iter().find(|c| c.name == "stubborn").is_none_or(|c| c.state != State::Running));
}

#[test]
fn ephemeral_containers_disappear_and_persistent_ones_can_be_restarted() {
    need_root!();
    let env = Env::start();
    let mut p = env.spec("keeper", "echo run-$$; exit 0");
    p.persistent = true;
    env.run(p);
    env.wait_for("keeper");
    wait_until("keeper stopped", || env.ps().iter().any(|c| c.name == "keeper" && c.state != State::Running));
    assert!(matches!(env.client().call(&Request::Start { target: "keeper".into() }).unwrap(), Response::Ok));
    env.wait_for("keeper");
    let logs = env.logs("keeper");
    assert_eq!(logs.matches("run-").count(), 2, "{logs}");
    assert!(matches!(env.client().call(&Request::Rm { target: "keeper".into(), force: false, keep_data: false }).unwrap(), Response::Ok));
    assert!(env.ps().iter().all(|c| c.name != "keeper"));
}

#[test]
fn rm_refuses_running_containers_unless_forced() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("busy", "sleep 30"));
    assert!(matches!(env.client().call(&Request::Rm { target: "busy".into(), force: false, keep_data: false }), Err(Error::Conflict(_))));
    assert!(matches!(env.client().call(&Request::Rm { target: "busy".into(), force: true, keep_data: false }).unwrap(), Response::Ok));
    assert!(env.ps().iter().all(|c| c.name != "busy"));
}

#[test]
fn containers_reach_each_other_over_the_bridge_with_isolated_port_spaces() {
    need_root!();
    let env = Env::start();
    let a = env.run(env.spec("srv-a", "while true; do echo from-a | nc -l -p 8080; done"));
    let b = env.run(env.spec("srv-b", "while true; do echo from-b | nc -l -p 8080; done"));
    let list = env.ps();
    let addr_of = |id| list.iter().find(|c| c.id == id).unwrap().address.unwrap();
    let (addr_a, addr_b) = (addr_of(a), addr_of(b));
    assert_ne!(addr_a, addr_b);
    let mut probe =
        env.spec("probe", &format!("for i in 1 2 3 4 5 6 7 8 9 10; do nc {addr_a} 8080 && nc {addr_b} 8080 && break; sleep 0.3; done"));
    probe.limits.pids_max = 64;
    env.run(probe);
    env.wait_for("probe");
    let log = env.logs("probe");
    assert!(log.contains("from-a") && log.contains("from-b"), "{log}");
}

#[test]
fn exec_runs_inside_a_running_container() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("target", "sleep 30"));
    let out_path = env.dir.path().join("exec.out");
    let out = fs::File::create(&out_path).unwrap();
    let null = fs::File::open("/dev/null").unwrap();
    let mut c = env.client();
    let req = Request::Exec {
        target: "target".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "cat /proc/1/comm; exit 4".into()],
        env: vec![],
        user: None,
        workdir: None,
        tty: false,
        timeout_secs: None,
    };
    c.send_with_fds(&req, &[&null, &out, &out]).unwrap();
    match c.read_response().unwrap() {
        Response::Exit { status } => assert_eq!(status, 4),
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read_to_string(out_path).unwrap().trim(), "init");
    let _ = null.as_raw_fd();
}

#[test]
fn exec_is_killed_when_it_exceeds_its_timeout() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("target", "sleep 30"));
    let null = fs::File::open("/dev/null").unwrap();
    let mut c = env.client();
    let req = Request::Exec {
        target: "target".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "trap '' TERM; sleep 30".into()],
        env: vec![],
        user: None,
        workdir: None,
        tty: false,
        timeout_secs: Some(1),
    };
    c.send_with_fds(&req, &[&null, &null, &null]).unwrap();
    let start = Instant::now();
    match c.read_response().unwrap() {
        Response::Exit { status } => assert_ne!(status, 0),
        other => panic!("{other:?}"),
    }
    assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
}

#[test]
fn exec_without_a_timeout_can_run_longer_than_a_typical_timeout() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("target", "sleep 30"));
    let null = fs::File::open("/dev/null").unwrap();
    let mut c = env.client();
    let req = Request::Exec {
        target: "target".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 1; exit 7".into()],
        env: vec![],
        user: None,
        workdir: None,
        tty: false,
        timeout_secs: None,
    };
    c.send_with_fds(&req, &[&null, &null, &null]).unwrap();
    match c.read_response().unwrap() {
        Response::Exit { status } => assert_eq!(status, 7),
        other => panic!("{other:?}"),
    }
}

#[test]
fn secrets_are_generated_once_and_mounted_read_only() {
    need_root!();
    let env = Env::start();
    let mut c = env.client();
    assert!(matches!(
        c.call(&Request::SecretEnsure { project: "app".into(), name: "pw".into(), generate: "password".into(), length: 20 }).unwrap(),
        Response::Ok
    ));
    let first = match c.call(&Request::SecretReveal { project: "app".into(), name: "pw".into() }).unwrap() {
        Response::Text { text } => text,
        other => panic!("{other:?}"),
    };
    assert_eq!(first.len(), 20);
    let mut s = env.spec("reader", "cat /run/secrets/pw; echo x > /run/secrets/pw 2>/dev/null; echo rc=$?");
    s.labels.project = Some("app".into());
    s.mounts.push(Mount::Secret { name: "pw".into(), dst: "/run/secrets/pw".into() });
    env.run(s);
    env.wait_for("reader");
    let log = env.logs("reader");
    assert!(log.contains(&first), "{log}");
    assert!(!log.contains("rc=0"), "{log}");
}

#[test]
fn idle_containers_are_reaped_after_their_timeout() {
    need_root!();
    let env = Env::start();
    let mut s = env.spec("session", "sleep 30");
    s.persistent = true;
    s.idle_timeout_secs = Some(1);
    env.run(s);
    wait_until("session reaped", || env.ps().iter().all(|c| c.name != "session"));
}

#[test]
fn exec_activity_resets_the_idle_timer() {
    need_root!();
    let env = Env::start();
    let mut s = env.spec("session", "sleep 30");
    s.persistent = true;
    s.idle_timeout_secs = Some(2);
    let id = env.run(s);
    std::thread::sleep(Duration::from_millis(1200));

    let null = fs::File::open("/dev/null").unwrap();
    let req = Request::Exec {
        target: id.to_string(),
        argv: vec!["/bin/sh".into(), "-c".into(), "true".into()],
        env: vec![],
        user: None,
        workdir: None,
        tty: false,
        timeout_secs: None,
    };
    let mut c = env.client();
    c.send_with_fds(&req, &[&null, &null, &null]).unwrap();
    c.read_response().unwrap();

    std::thread::sleep(Duration::from_millis(1200));
    assert!(env.ps().iter().any(|c| c.name == "session"), "activity should have reset the idle timer");
    wait_until("session eventually reaped", || env.ps().iter().all(|c| c.name != "session"));
}

#[test]
fn containers_without_an_idle_timeout_are_never_reaped() {
    need_root!();
    let env = Env::start();
    env.run(env.spec("forever", "sleep 30"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(env.ps().iter().any(|c| c.name == "forever"));
}

#[test]
fn restart_policy_relaunches_failed_containers() {
    need_root!();
    let env = Env::start();
    let mut s = env.spec("flaky", "echo attempt; exit 1");
    s.persistent = true;
    s.restart = collocate_core::spec::RestartPolicy::OnFailure { max: 2 };
    env.run(s);
    wait_until("three attempts", || env.logs("flaky").matches("attempt").count() >= 3);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(env.logs("flaky").matches("attempt").count(), 3);
}

#[test]
fn stats_expose_cgroup_accounting() {
    need_root!();
    let env = Env::start();
    let mut s = env.spec("accounted", "sleep 30");
    s.limits.memory = Some(64 << 20);
    s.limits.cpus_milli = Some(500);
    s.labels.project = Some("p".into());
    env.run(s);
    match env.client().call(&Request::Stats { project: Some("p".into()) }).unwrap() {
        Response::Stats(v) => {
            assert_eq!(v.len(), 1);
            assert_eq!(v[0].memory_max, Some(64 << 20));
            assert_eq!(v[0].cpu_limit_milli, Some(500));
            assert!(v[0].pids >= 2);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_restarted_daemon_readopts_running_containers() {
    need_root!();
    let dir;
    let sock;
    let cg_root;
    {
        let env = Env::start();
        env.run(env.spec("survivor", "sleep 60"));
        dir = env.dir.path().to_path_buf();
        sock = env.sock.clone();
        cg_root = env.cg_root.clone();
        let mut c = env.client();
        let _ = c.call(&Request::Shutdown);
        let mut env = env;
        if let Some(t) = env.thread.take() {
            t.join().unwrap();
        }
        std::mem::forget(env);
    }
    let cfg = Config {
        state_dir: dir.join("state"),
        run_dir: dir.join("run"),
        root_mode: RootModeSetting::BindRo,
        move_self_to_supervisor: false,
        init_path: init_binary(),
        cgroup_root: cg_root.clone(),
        bridge: format!("colt{}", std::process::id() % 10000),
        subnet: "10.213.0.0/24".into(),
        ..Config::default()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || {
        let mut d = Daemon::new(cfg).unwrap();
        tx.send(()).unwrap();
        d.run().unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut c = Client::connect(&sock).unwrap();
    let list = match c.call(&Request::Ps { all: true, project: None }).unwrap() {
        Response::Containers(l) => l,
        other => panic!("{other:?}"),
    };
    let s = list.iter().find(|c| c.name == "survivor").unwrap();
    assert_eq!(s.state, State::Running);
    assert!(matches!(c.call(&Request::Kill { target: "survivor".into(), signal: 9 }).unwrap(), Response::Ok));
    wait_until(
        "survivor to die",
        || matches!(Client::connect(&sock).unwrap().call(&Request::Ps { all: true, project: None }).unwrap(), Response::Containers(l) if l.iter().all(|c| c.name != "survivor" || c.state != State::Running)),
    );
    let _ = Client::connect(&sock).unwrap().call(&Request::Shutdown);
    t.join().unwrap();
    let _ = fs::write(cg_root.join("cgroup.kill"), "1");
    std::thread::sleep(Duration::from_millis(100));
    for sub in ["collocate.slice/containers", "collocate.slice/supervisor", "collocate.slice"] {
        if let Ok(rd) = fs::read_dir(cg_root.join(sub)) {
            for e in rd.flatten() {
                let _ = fs::remove_dir(e.path());
            }
        }
        let _ = fs::remove_dir(cg_root.join(sub));
    }
    let _ = fs::remove_dir(&cg_root);
    let _ = std::process::Command::new("ip").args(["link", "del", &format!("colt{}", std::process::id() % 10000)]).output();
}

fn fetch(addr: std::net::Ipv4Addr, port: u16) -> Option<String> {
    use std::io::Read;
    let mut s = std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from((addr, port)), Duration::from_millis(800)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    Some(buf.trim().to_string())
}

#[test]
fn published_ports_are_reachable_through_dnat() {
    need_root!();
    let env = Env::start();
    let mut s = env.spec("server", "while true; do echo pong | nc -l -p 8080; done");
    s.net.publish.push(collocate_core::net::Publish::parse("18080:8080").unwrap());
    env.run(s);
    let gateway: std::net::Ipv4Addr = "10.213.0.1".parse().unwrap();
    wait_until("published port", || fetch(gateway, 18080).as_deref() == Some("pong"));
    assert!(fetch(gateway, 18081).is_none(), "unpublished ports must not be reachable");
}

#[test]
fn load_balancers_spread_connections_and_drop_dead_backends() {
    need_root!();
    let env = Env::start();
    for (name, reply) in [("app-a", "A"), ("app-b", "B")] {
        let mut s = env.spec(name, &format!("while true; do echo {reply} | nc -l -p 8080; done"));
        s.labels.project = Some("shop".into());
        s.labels.service = Some("app".into());
        env.run(s);
    }
    let lb = collocate_core::request::LbSpec {
        project: "shop".into(),
        name: "web".into(),
        proto: collocate_core::net::Proto::Tcp,
        listen: 80,
        publish: vec![],
        backend_service: "app".into(),
        backend_port: 8080,
        algorithm: collocate_core::net::Algorithm::RoundRobin,
        on_no_backends: collocate_core::net::NoBackends::Reject,
        drain_secs: 0,
        vip: None,
    };
    assert!(matches!(env.client().call(&Request::LbSet { lb }).unwrap(), Response::Ok));
    let vip = match env.client().call(&Request::LbList).unwrap() {
        Response::Lbs(l) => {
            assert_eq!(l[0].backends.len(), 2);
            l[0].vip
        }
        other => panic!("{other:?}"),
    };
    wait_until("both backends to answer", || {
        let seen: std::collections::HashSet<String> = (0..12).filter_map(|_| fetch(vip, 80)).collect();
        seen.contains("A") && seen.contains("B")
    });

    env.client().call(&Request::Kill { target: "app-a".into(), signal: 9 }).unwrap();
    wait_until(
        "dead backend to leave the rotation",
        || matches!(env.client().call(&Request::LbList).unwrap(), Response::Lbs(l) if l[0].backends.len() == 1),
    );
    let replies: Vec<Option<String>> = (0..10).map(|_| fetch(vip, 80)).collect();
    assert!(replies.iter().flatten().all(|r| r == "B"), "the dead backend must receive no traffic: {replies:?}");
    assert!(replies.iter().any(|r| r.is_some()), "the surviving backend must still answer: {replies:?}");
}
