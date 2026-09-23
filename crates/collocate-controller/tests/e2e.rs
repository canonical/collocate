use collocate_compose::model::ComposeFile;
use collocate_compose::up::UpOptions;
use collocate_controller::controller::Controller;
use collocate_core::client::{Api, Client};
use collocate_core::request::{Request, Response, State};
use collocated::config::{Config, RootModeSetting};
use collocated::daemon::Daemon;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

macro_rules! need_root {
    () => {
        if !is_root() {
            return;
        }
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    };
}

fn init_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = root.join("target/x86_64-unknown-linux-musl/debug/collocate-init");
    if !bin.exists() {
        assert!(std::process::Command::new("cargo").args(["build", "-p", "collocate-init"]).current_dir(&root).status().unwrap().success());
    }
    bin
}

const APPLETS: [&str; 3] = ["sh", "sleep", "true"];

struct Env {
    dir: tempfile::TempDir,
    sock: PathBuf,
    cg_root: PathBuf,
    bridge: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Env {
    fn start() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let run = dir.path().join("run");
        fs::create_dir_all(&run).unwrap();
        let rootfs = state.join("images/noble/b1/rootfs");
        for d in ["bin", "etc", "proc", "sys", "dev", "run", "tmp", ".collocate"] {
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
        symlink("b1", state.join("images/noble/latest")).unwrap();
        let cg_root = PathBuf::from(format!("/sys/fs/cgroup/collocate-ctlr-{}", std::process::id()));
        fs::create_dir(&cg_root).unwrap();
        fs::write(cg_root.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
        let bridge = format!("ctlr{}", std::process::id() % 10000);
        let cfg = Config {
            state_dir: state,
            run_dir: run,
            root_mode: RootModeSetting::BindRo,
            move_self_to_supervisor: false,
            init_path: init_binary(),
            cgroup_root: cg_root.clone(),
            bridge: bridge.clone(),
            subnet: "10.215.0.0/24".into(),
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
        Env { dir, sock, cg_root, bridge, thread: Some(thread) }
    }

    fn client(&self) -> Client<std::os::unix::net::UnixStream> {
        Client::connect(&self.sock).unwrap()
    }

    fn containers(&self) -> Vec<collocate_core::request::ContainerInfo> {
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
        let _ = std::process::Command::new("ip").args(["link", "del", &self.bridge]).output();
        let _ = self.dir.path();
    }
}

fn stack(cmd: &str, replicas: u32) -> String {
    format!(
        "version: 1\nproject: demo\nservices:\n  web:\n    series: \"24.04\"\n    replicas: {replicas}\n    command: [\"/bin/sh\", \"-c\", \"{cmd}\"]\n"
    )
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn tick_until(controller: &mut Controller, api: &mut dyn Api, what: &str, mut f: impl FnMut(&mut Controller, &mut dyn Api) -> bool) {
    let start = Instant::now();
    loop {
        let _ = controller.tick(api, now_secs()).unwrap();
        if f(controller, api) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn running_web_count(env: &Env) -> usize {
    env.containers().iter().filter(|c| c.name.starts_with("demo-web-") && c.state == State::Running).count()
}

#[test]
fn the_real_controller_brings_a_service_up_and_performs_a_rolling_update() {
    need_root!();
    let env = Env::start();
    let opts = UpOptions {
        subnet: "10.215.0.0/24".into(),
        base_dir: PathBuf::from("."),
        regenerate_secrets: None,
        dry_run: false,
        ready_timeout: Duration::from_secs(10),
        poll_interval: Duration::from_millis(100),
    };
    let file = ComposeFile::load(&stack("sleep 60", 2)).unwrap();
    let mut controller = Controller::new(file, opts).unwrap();
    let mut client = env.client();

    tick_until(&mut controller, &mut client, "2 replicas running", |_, _| running_web_count(&env) == 2);
    let original: std::collections::BTreeSet<String> =
        env.containers().iter().filter(|c| c.name.starts_with("demo-web-")).map(|c| c.name.clone()).collect();
    assert_eq!(original.len(), 2);

    let new_file = ComposeFile::load(&stack("echo v2; sleep 60", 2)).unwrap();
    controller.set_file(new_file);
    tick_until(&mut controller, &mut client, "the rollout to replace both replicas", |_, api| {
        let running: Vec<String> = env
            .containers()
            .iter()
            .filter(|c| c.name.starts_with("demo-web-") && c.state == State::Running)
            .map(|c| c.name.clone())
            .collect();
        running.len() == 2
            && running.iter().all(|n| {
                matches!(
                    api.call(Request::Logs { target: n.clone(), tail: None, offset: None }),
                    Ok(Response::Log { data, .. }) if data.contains("v2")
                )
            })
    });
}
