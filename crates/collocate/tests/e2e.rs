use collocated::config::{Config, RootModeSetting};
use collocated::daemon::Daemon;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const CLI: &str = env!("CARGO_BIN_EXE_collocate");

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn init_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = root.join("target/x86_64-unknown-linux-musl/debug/collocate-init");
    if !bin.exists() {
        assert!(Command::new("cargo").args(["build", "-p", "collocate-init"]).current_dir(&root).status().unwrap().success());
    }
    bin
}

struct Env {
    dir: tempfile::TempDir,
    sock: PathBuf,
    cg_root: PathBuf,
    bridge: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

const APPLETS: [&str; 12] = ["sh", "cat", "ls", "echo", "grep", "id", "hostname", "touch", "wc", "sleep", "env", "true"];

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
        let cg_root = PathBuf::from(format!("/sys/fs/cgroup/collocate-cli-{}", std::process::id()));
        fs::create_dir(&cg_root).unwrap();
        fs::write(cg_root.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
        let bridge = format!("colc{}", std::process::id() % 10000);
        let cfg = Config {
            state_dir: state,
            run_dir: run,
            root_mode: RootModeSetting::BindRo,
            move_self_to_supervisor: false,
            init_path: init_binary(),
            cgroup_root: cg_root.clone(),
            bridge: bridge.clone(),
            subnet: "10.214.0.0/24".into(),
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

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(CLI)
            .arg("--host")
            .arg(&self.sock)
            .args(args)
            .env("COLLOCATE_CGROUP_ROOT", self.cg_root.join("collocate.slice/containers"))
            .current_dir(self.dir.path())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        assert!(o.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn err(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        assert!(o.status.success(), "{args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    fn stop_daemon(&mut self) {
        if let Ok(mut c) = collocate_core::client::Client::connect(&self.sock) {
            let _ = c.call(&collocate_core::request::Request::Shutdown);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = Command::new(CLI).arg("--host").arg(&self.sock).arg("info").output();
        if let Ok(mut c) = collocate_core::client::Client::connect(&self.sock) {
            let _ = c.call(&collocate_core::request::Request::Shutdown);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = fs::write(self.cg_root.join("cgroup.kill"), "1");
        std::thread::sleep(Duration::from_millis(100));
        for sub in ["collocate.slice/containers", "collocate.slice/supervisor", "collocate.slice"] {
            if let Ok(rd) = fs::read_dir(self.cg_root.join(sub)) {
                for e in rd.flatten() {
                    let _ = fs::remove_dir(e.path());
                }
            }
            let _ = fs::remove_dir(self.cg_root.join(sub));
        }
        let _ = fs::remove_dir(&self.cg_root);
        let _ = Command::new("ip").args(["link", "del", &self.bridge]).output();
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(15), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
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

#[test]
fn container_lifecycle_through_the_binary() {
    need_root!();
    let env = Env::start();
    let id =
        env.ok(&["run", "-d", "--series", "24.04", "--name", "web", "--memory", "64m", "--", "/bin/sh", "-c", "echo hello-cli; sleep 60"]);
    assert_eq!(id.trim().len(), 12);

    let list = env.ok(&["list"]);
    assert!(list.contains("web") && list.contains("running"), "{list}");
    let list_json = env.ok(&["--format", "json", "list"]);
    let parsed: serde_json::Value = serde_json::from_str(&list_json).unwrap();
    assert_eq!(parsed[0]["name"], "web");
    assert_eq!(env.ok(&["ls"]), list);

    wait_until("log line", || env.ok(&["logs", "web"]).contains("hello-cli"));

    let status = env.ok(&["status"]);
    assert!(status.starts_with("NAME"), "{status}");
    let row = status.lines().find(|l| l.starts_with("web")).unwrap();
    assert!(row.contains("running") && row.contains("sleep") && row.contains("10.214.0."), "{row}");

    assert_eq!(env.ok(&["exec", "web", "--", "cat", "/proc/1/comm"]).trim(), "init");

    env.ok(&["stop", "-t", "3", "web"]);
    wait_until("stopped", || !env.ok(&["list"]).contains("running"));

    let refused = env.cli(&["delete", "web"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"), "{}", String::from_utf8_lossy(&refused.stderr));
    assert!(env.ok(&["list", "-a"]).contains("web"));

    env.ok(&["-y", "rm", "web"]);
    assert!(!env.ok(&["list", "-a"]).contains("web"));
}

#[test]
fn list_shows_an_empty_state_message_with_no_containers() {
    need_root!();
    let env = Env::start();
    assert_eq!(env.ok(&["list"]).trim(), "No containers found.");
    assert_eq!(env.ok(&["--format", "json", "list"]).trim(), "[]");
}

#[test]
fn foreground_run_streams_output_and_returns_the_exit_code() {
    need_root!();
    let env = Env::start();
    let o = env.cli(&["run", "--series", "24.04", "--name", "fg", "--", "/bin/sh", "-c", "echo out-line; exit 4"]);
    assert_eq!(o.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&o.stdout).contains("out-line"));
}

#[test]
fn errors_use_the_documented_exit_codes() {
    need_root!();
    let env = Env::start();
    assert_eq!(env.cli(&["stop", "ghost"]).status.code(), Some(3));
    assert_eq!(env.cli(&["kill", "ghost"]).status.code(), Some(3));
    assert_eq!(env.cli(&["run", "--memory", "lots", "--", "/bin/true"]).status.code(), Some(2));
    let unreachable = Command::new(CLI).args(["--host", "/nonexistent.sock", "ps"]).output().unwrap();
    assert_eq!(unreachable.status.code(), Some(4));
    env.ok(&["run", "-d", "--series", "24.04", "--name", "dup", "--", "/bin/sleep", "30"]);
    assert_eq!(env.cli(&["run", "-d", "--series", "24.04", "--name", "dup", "--", "/bin/true"]).status.code(), Some(5));
}

#[test]
fn secrets_roundtrip_through_the_binary() {
    need_root!();
    let env = Env::start();
    let f = env.dir.path().join("value.txt");
    fs::write(&f, "s3cret\n").unwrap();
    env.ok(&["secret", "set", "app", "pw", "--from-file", f.to_str().unwrap()]);
    assert_eq!(env.ok(&["secret", "list"]).trim(), "app/pw");
    assert_eq!(env.ok(&["secret", "ls"]).trim(), "app/pw");
    assert_eq!(env.ok(&["secret", "reveal", "app", "pw"]).trim(), "s3cret");

    let refused = env.cli(&["secret", "delete", "app", "pw"]);
    assert!(!refused.status.success());
    assert_eq!(env.ok(&["secret", "list"]).trim(), "app/pw");

    env.ok(&["-y", "secret", "rm", "app", "pw"]);
    assert_eq!(env.ok(&["secret", "list"]).trim(), "No secrets found.");
}

#[test]
fn node_reinit_and_adopt_require_the_daemon_to_be_stopped_first() {
    need_root!();
    let mut env = Env::start();
    let refused = env.cli(&["node", "reinit"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("stop collocated"), "{}", String::from_utf8_lossy(&refused.stderr));

    env.stop_daemon();
    let out = env.ok(&["node", "reinit"]);
    assert!(out.starts_with("node:"), "{out}");
    let first_uuid = out.lines().nth(1).unwrap().to_string();

    let adopted = env.ok(&["node", "adopt"]);
    assert_eq!(adopted.lines().nth(1).unwrap(), first_uuid, "adopt should keep the same node uuid");

    let reinited = env.ok(&["node", "reinit"]);
    assert_ne!(reinited.lines().nth(1).unwrap(), first_uuid, "reinit should generate a fresh node uuid");
}

#[test]
fn load_balancer_roundtrip_through_the_binary() {
    need_root!();
    let env = Env::start();
    assert_eq!(env.ok(&["load-balancer", "list"]).trim(), "No load balancers found.");
    env.ok(&["load-balancer", "create", "shop", "web", "--listen", "80", "--backend-service", "server", "--backend-port", "8080"]);
    let list = env.ok(&["lb", "list"]);
    assert!(list.contains("shop/web"), "{list}");
    let show = env.ok(&["lb", "show", "shop", "web"]);
    assert!(show.contains("name: shop/web") && show.contains("listen: 80"), "{show}");

    let refused = env.cli(&["lb", "delete", "shop", "web"]);
    assert!(!refused.status.success());
    assert!(env.ok(&["lb", "list"]).contains("shop/web"));

    env.ok(&["-y", "lb", "rm", "shop", "web"]);
    assert_eq!(env.ok(&["lb", "list"]).trim(), "No load balancers found.");
}

#[test]
fn compose_up_plan_and_down() {
    need_root!();
    let env = Env::start();
    let file = env.dir.path().join("collocate-compose.yaml");
    fs::write(
        &file,
        "version: 1\nproject: demo\nservices:\n  db:\n    series: \"24.04\"\n    command: [\"/bin/sleep\", \"60\"]\n    env:\n      PW: ${secrets.pw}\n  app:\n    series: \"24.04\"\n    depends_on: [db]\n    command: [\"/bin/sh\", \"-c\", \"echo db=$DB; sleep 60\"]\n    env:\n      DB: ${services.db.address}\nsecrets:\n  pw:\n    generate: password\n    length: 12\n",
    )
    .unwrap();
    let f = file.to_str().unwrap();
    let plan = env.ok(&["plan", "-f", f]);
    assert!(plan.contains("create") && plan.contains("demo-db-1"), "{plan}");
    let out = env.err(&["up", "-f", f, "--timeout", "20"]);
    assert!(out.contains("Created demo-app-1"), "{out}");
    let list = env.ok(&["list", "--project", "demo"]);
    assert!(list.contains("demo-db-1") && list.contains("demo-app-1"), "{list}");
    let db_addr = list.lines().find(|l| l.contains("demo-db-1")).unwrap().split_whitespace().nth(3).unwrap().to_string();
    wait_until("app log", || env.ok(&["logs", "demo-app-1"]).contains(&format!("db={db_addr}")));
    assert!(env.ok(&["plan", "-f", f]).lines().all(|l| l.starts_with("keep")));
    let again = env.err(&["--verbose", "up", "-f", f, "--timeout", "20"]);
    assert!(!again.contains("Created") && again.contains("already up to date"), "{again}");

    let refused = env.cli(&["down", "-f", f]);
    assert!(!refused.status.success());
    assert!(env.ok(&["list", "-a", "--project", "demo"]).contains("demo-db-1"));

    let down = env.err(&["-y", "down", "-f", f]);
    assert!(down.contains("Removed demo-app-1") && down.contains("Removed demo-db-1"), "{down}");
    assert!(!env.ok(&["list", "-a"]).contains("demo-"));
}

#[test]
fn doctor_and_info_report_the_environment() {
    need_root!();
    let env = Env::start();
    let info: serde_json::Value = serde_json::from_str(env.ok(&["info"]).trim()).unwrap();
    assert_eq!(info["root_mode"], "bind-ro");
    assert!(info["node_uuid"].as_str().unwrap().len() >= 16);
    let doctor = env.cli(&["doctor"]);
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(text.contains("cgroup2") && text.contains("daemon: reachable"), "{text}");
}

fn docker_archive_with_busybox(tag: &str, cmd: &[&str]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    use tar::{Builder, EntryType, Header};
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let mut layer = Builder::new(Vec::new());
    let add_dir = |b: &mut Builder<Vec<u8>>, p: &str| {
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o755);
        h.set_entry_type(EntryType::Directory);
        h.set_cksum();
        b.append_data(&mut h, p, std::io::empty()).unwrap();
    };
    for d in ["bin/", "etc/", "proc/", "sys/", "dev/", "run/", "tmp/", ".collocate/"] {
        add_dir(&mut layer, d);
    }
    let add_file = |b: &mut Builder<Vec<u8>>, p: &str, data: &[u8], mode: u32| {
        let mut h = Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, p, data).unwrap();
    };
    for f in ["etc/resolv.conf", "etc/hosts", "etc/hostname", ".collocate/init"] {
        add_file(&mut layer, f, b"", 0o644);
    }
    add_file(&mut layer, "etc/passwd", b"root:x:0:0:root:/root:/bin/sh\n", 0o644);
    add_file(&mut layer, "etc/group", b"root:x:0:\n", 0o644);
    add_file(&mut layer, "bin/busybox", &fs::read("/usr/bin/busybox").unwrap(), 0o755);
    for a in APPLETS {
        let mut h = Header::new_gnu();
        h.set_size(0);
        h.set_mode(0o777);
        h.set_entry_type(EntryType::Symlink);
        h.set_cksum();
        layer.append_link(&mut h, format!("bin/{a}"), "busybox").unwrap();
    }
    let layer_bytes = layer.into_inner().unwrap();
    let diff_id = format!("sha256:{}", hex(&Sha256::digest(&layer_bytes)));
    let config = serde_json::json!({
        "architecture": "amd64", "os": "linux",
        "config": {"Cmd": cmd, "Env": ["PATH=/bin", "FROM_IMAGE=yes"], "WorkingDir": "/tmp"},
        "rootfs": {"type": "layers", "diff_ids": [diff_id]}
    })
    .to_string();
    let cfg_name = format!("{}.json", hex(&Sha256::digest(config.as_bytes())));
    let mut outer = Builder::new(Vec::new());
    let mut put = |name: &str, data: &[u8]| {
        let mut h = Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(EntryType::Regular);
        h.set_cksum();
        outer.append_data(&mut h, name, data).unwrap();
    };
    put("layer0/layer.tar", &layer_bytes);
    put(&cfg_name, config.as_bytes());
    put(
        "manifest.json",
        serde_json::json!([{"Config": cfg_name, "RepoTags": [tag], "Layers": ["layer0/layer.tar"]}]).to_string().as_bytes(),
    );
    outer.into_inner().unwrap()
}

#[test]
fn imported_docker_images_run_with_their_own_root_and_config() {
    need_root!();
    let env = Env::start();
    let archive = env.dir.path().join("app.tar");
    fs::write(&archive, docker_archive_with_busybox("myapp:1", &["/bin/sh", "-c", "echo image=$FROM_IMAGE; pwd; exit 6"])).unwrap();
    let state = env.dir.path().join("state");
    let imp = env.err(&["--state-dir", state.to_str().unwrap(), "image", "import", archive.to_str().unwrap()]);
    assert!(imp.contains("Imported myapp:1"), "{imp}");
    assert!(env.ok(&["--state-dir", state.to_str().unwrap(), "image", "list"]).contains("myapp:1"));
    assert!(env.ok(&["--state-dir", state.to_str().unwrap(), "image", "ls"]).contains("myapp:1"));
    let shown = env.ok(&["--state-dir", state.to_str().unwrap(), "image", "show", "myapp:1"]);
    assert!(shown.contains("name: myapp:1") && shown.contains("layers: 1"), "{shown}");

    let o = env.cli(&["--state-dir", state.to_str().unwrap(), "run", "--image", "myapp:1", "--name", "fromimage"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(6), "{out} {}", String::from_utf8_lossy(&o.stderr));
    assert!(out.contains("image=yes"), "{out}");
    assert!(out.contains("/tmp"), "{out}");

    let o = env.cli(&[
        "--state-dir",
        state.to_str().unwrap(),
        "run",
        "--image",
        "myapp:1",
        "--name",
        "override",
        "-e",
        "FROM_IMAGE=overridden",
        "--",
        "echo",
        "hello",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "hello");
}
