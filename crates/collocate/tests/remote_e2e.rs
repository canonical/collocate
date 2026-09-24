mod common;

use collocated::config::{Config, RootModeSetting};
use collocated::daemon::Daemon;
use common::{docker_archive_with_busybox, APPLETS};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CLI: &str = env!("CARGO_BIN_EXE_collocate");

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn init_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.join("target/x86_64-unknown-linux-musl/debug/collocate-init")
}

struct Env {
    dir: tempfile::TempDir,
    sock: PathBuf,
    cg_root: PathBuf,
    bridge: String,
    port: u16,
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
        for f in ["etc/resolv.conf", "etc/hosts", "etc/hostname", "etc/app.conf", ".collocate/init"] {
            fs::write(rootfs.join(f), "").unwrap();
        }
        fs::write(rootfs.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();
        fs::write(rootfs.join("etc/group"), "root:x:0:\n").unwrap();
        fs::copy("/usr/bin/busybox", rootfs.join("bin/busybox")).unwrap();
        for a in APPLETS {
            symlink("busybox", rootfs.join("bin").join(a)).unwrap();
        }
        symlink("b1", state.join("images/noble/latest")).unwrap();
        let cg_root = PathBuf::from(format!("/sys/fs/cgroup/collocate-remote-{}", std::process::id()));
        fs::create_dir(&cg_root).unwrap();
        fs::write(cg_root.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
        let bridge = format!("colr{}", std::process::id() % 10000);
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let cfg = Config {
            state_dir: state,
            run_dir: run,
            root_mode: RootModeSetting::BindRo,
            move_self_to_supervisor: false,
            init_path: init_binary(),
            cgroup_root: cg_root.clone(),
            bridge: bridge.clone(),
            subnet: "10.217.0.0/24".into(),
            https_address: Some(format!("127.0.0.1:{port}")),
            ..Config::default()
        };
        let sock = cfg.socket();
        let config_path = dir.path().join("collocated.toml");
        fs::write(&config_path, format!("state_dir = \"{}\"\n", cfg.state_dir.display())).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let trust_dir = cfg.state_dir.join("trust");
        let thread = std::thread::spawn(move || {
            let mut d = Daemon::new(cfg).expect("daemon");
            d.set_config_path(config_path);
            tx.send(()).unwrap();
            d.run().expect("run");
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let identity = collocate_trust::TrustStore::open(&trust_dir).unwrap().server_identity().unwrap().expect("server identity");
        let config = collocate_remote::tls::server_config(&identity).unwrap();
        let gateway = collocate_gateway::Gateway::new(sock.clone(), identity.fingerprint.clone());
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                gateway.try_serve(stream, config.clone());
            }
        });
        Env { dir, sock, cg_root, bridge, port, thread: Some(thread) }
    }

    fn client_dir(&self, name: &str) -> PathBuf {
        let d = self.dir.path().join("clients").join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn cli(&self, client: Option<&str>, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(CLI);
        cmd.arg("--host").arg(&self.sock).args(args).current_dir(self.dir.path()).env_remove("COLLOCATE_REMOTE");
        match client {
            Some(c) => cmd.env("COLLOCATE_CONFIG_DIR", self.client_dir(c)),
            None => cmd.env("COLLOCATE_CONFIG_DIR", self.client_dir("admin-local")),
        };
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn ok(&self, client: Option<&str>, args: &[&str]) -> String {
        let o = self.cli(client, args, None);
        assert!(
            o.status.success(),
            "{client:?} {args:?} failed: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn token(&self, name: &str, extra: &[&str]) -> String {
        let mut args = vec!["trust", "add", name];
        args.extend_from_slice(extra);
        self.ok(None, &args).trim().to_string()
    }

    fn enroll(&self, client: &str, token: &str) {
        self.ok(Some(client), &["remote", "add", "prod", token]);
        self.ok(Some(client), &["remote", "switch", "prod"]);
    }
}

impl Drop for Env {
    fn drop(&mut self) {
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

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn remote_clients_drive_collocate_through_the_gateway() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    let env = Env::start();
    let local_info: serde_json::Value = serde_json::from_str(&env.ok(None, &["info", "--format", "json"])).unwrap();
    assert_eq!(local_info["https_address"], format!("127.0.0.1:{}", env.port));
    let fingerprint = local_info["server_fingerprint"].as_str().unwrap().to_string();

    let ci = env.token("ci", &["--role", "operator"]);
    let decoded = collocate_trust::Token::decode(&ci).unwrap();
    assert_eq!(decoded.fingerprint, fingerprint);
    assert_eq!(decoded.addresses, vec![format!("127.0.0.1:{}", env.port)]);
    assert_eq!(decoded.expires_at, None);
    assert!(env.ok(None, &["trust", "token", "list"]).contains("ci"));
    env.enroll("ci", &ci);
    assert!(env.ok(None, &["trust", "list"]).contains("ci"));
    assert!(!env.ok(None, &["trust", "token", "list"]).contains("ci"));
    let reuse = env.cli(Some("thief"), &["remote", "add", "prod", &ci], None);
    assert_eq!(code(&reuse), 9, "{}", err(&reuse));

    let info: serde_json::Value = serde_json::from_str(&env.ok(Some("ci"), &["info", "--format", "json"])).unwrap();
    assert_eq!(info["server_fingerprint"], fingerprint.as_str());
    env.ok(Some("ci"), &["run", "-d", "--series", "24.04", "--name", "r1", "--", "/bin/sh", "-c", "echo remote-log-line; sleep 120"]);
    assert!(env.ok(Some("ci"), &["list"]).contains("r1"));
    let exec = env.cli(Some("ci"), &["exec", "r1", "--", "/bin/sh", "-c", "echo out; echo err >&2; cat; exit 7"], Some("piped-in\n"));
    assert_eq!(code(&exec), 7, "{}", err(&exec));
    assert_eq!(String::from_utf8_lossy(&exec.stdout), "out\npiped-in\n");
    assert_eq!(err(&exec), "err\n");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !env.ok(Some("ci"), &["logs", "r1"]).contains("remote-log-line") {
        assert!(Instant::now() < deadline, "logs never showed up");
        std::thread::sleep(Duration::from_millis(200));
    }
    let src = env.dir.path().join("upload.txt");
    fs::write(&src, "copied over https\n").unwrap();
    env.ok(Some("ci"), &["cp", src.to_str().unwrap(), "r1:/tmp/upload.txt"]);
    let back = env.dir.path().join("download.txt");
    env.ok(Some("ci"), &["cp", "r1:/tmp/upload.txt", back.to_str().unwrap()]);
    assert_eq!(fs::read_to_string(&back).unwrap(), "copied over https\n");

    let archive = env.dir.path().join("app.tar");
    fs::write(&archive, docker_archive_with_busybox("remoteapp:1", &["/bin/sh", "-c", "echo image=$FROM_IMAGE"])).unwrap();
    env.ok(Some("ci"), &["image", "import", archive.to_str().unwrap()]);
    let piped = env.cli(Some("ci"), &["image", "import", "-"], None);
    assert_ne!(code(&piped), 0);
    assert!(env.ok(Some("ci"), &["image", "list"]).contains("remoteapp:1"));
    assert!(env.ok(Some("ci"), &["run", "--image", "remoteapp:1", "--name", "fromimg"]).contains("image=yes"));

    let project = env.dir.path().join("proj");
    fs::create_dir_all(project.join("templates")).unwrap();
    fs::write(project.join("templates/app.conf"), "listen=${services.web.address}\n").unwrap();
    fs::write(
        project.join("collocate-compose.yaml"),
        "version: 1\nproject: web\nconfigs:\n  app:\n    template: ./templates/app.conf\nservices:\n  web:\n    series: \"24.04\"\n    command: [/bin/sleep, \"120\"]\n    configs:\n      app: /etc/app.conf\n",
    )
    .unwrap();
    let file = project.join("collocate-compose.yaml");
    env.ok(Some("ci"), &["up", "-f", file.to_str().unwrap()]);
    let conf = env.ok(Some("ci"), &["exec", "web-web-1", "--", "cat", "/etc/app.conf"]);
    assert!(conf.starts_with("listen=10.217.0."), "{conf}");
    assert!(!project.join(".collocate").exists());

    let curl_dir = env.client_dir("curl");
    let viewer = env.token("dash", &["--role", "viewer"]);
    env.enroll("curl", &viewer);
    let listed = env.ok(Some("curl"), &["list", "--format", "json"]);
    assert!(listed.contains("r1") && listed.contains("web-web-1"));
    let denied = env.cli(Some("curl"), &["exec", "r1", "--", "true"], None);
    assert_eq!(code(&denied), 9, "{}", err(&denied));
    assert!(err(&denied).contains("viewer"));
    let denied = env.cli(Some("curl"), &["stop", "r1"], None);
    assert_eq!(code(&denied), 9);
    let denied = env.cli(Some("curl"), &["trust", "list"], None);
    assert_eq!(code(&denied), 9);
    let url = format!("https://127.0.0.1:{}/1.0/call", env.port);
    let curl = Command::new("curl")
        .args(["-sS", "-k", "--cert"])
        .arg(curl_dir.join("client.crt"))
        .arg("--key")
        .arg(curl_dir.join("client.key"))
        .args(["-X", "POST", "-d", r#"{"verb":"ps","all":true,"project":null}"#, &url])
        .output()
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&curl.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&curl.stdout)));
    assert_eq!(body["kind"], "containers", "{body}");
    let logs = Command::new("curl")
        .args(["-sS", "-k", "--cert"])
        .arg(curl_dir.join("client.crt"))
        .arg("--key")
        .arg(curl_dir.join("client.key"))
        .arg(format!("https://127.0.0.1:{}/1.0/logs?target=r1", env.port))
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&logs.stdout).contains("remote-log-line"));
    let anonymous = Command::new("curl").args(["-sS", "-k", "-X", "POST", "-d", "{\"verb\":\"info\"}", &url]).output().unwrap();
    assert!(String::from_utf8_lossy(&anonymous.stdout).contains("client certificate"));

    let scoped = env.token("webteam", &["--projects", "web"]);
    env.enroll("web", &scoped);
    let seen = env.ok(Some("web"), &["list", "--format", "json"]);
    assert!(seen.contains("web-web-1") && !seen.contains("\"r1\""), "{seen}");
    assert_eq!(code(&env.cli(Some("web"), &["stop", "r1"], None)), 9);
    assert_eq!(code(&env.cli(Some("web"), &["run", "-d", "--series", "24.04", "--name", "stray", "--", "/bin/true"], None)), 9);
    env.ok(Some("web"), &["run", "-d", "--series", "24.04", "--project", "web", "--name", "scoped", "--", "/bin/sleep", "60"]);
    env.ok(Some("web"), &["secret", "set", "web", "key", "--from-file", src.to_str().unwrap()]);
    assert_eq!(code(&env.cli(Some("web"), &["secret", "set", "other", "key", "--from-file", src.to_str().unwrap()], None)), 9);
    assert_eq!(code(&env.cli(Some("web"), &["image", "prune", "--yes"], None)), 9);

    let short = env.token("short", &["--expiry", "1s"]);
    std::thread::sleep(Duration::from_secs(2));
    let expired = env.cli(Some("late"), &["remote", "add", "prod", &short], None);
    assert_eq!(code(&expired), 9, "{}", err(&expired));
    assert!(err(&expired).contains("expired"));

    let admin = env.token("ops", &["--role", "admin"]);
    env.enroll("ops", &admin);
    let dump = env.ok(Some("ops"), &["init", "--dump"]);
    assert!(dump.contains(&format!("127.0.0.1:{}", env.port)), "{dump}");
    let reapplied = env.cli(Some("ops"), &["init", "--preseed"], Some(&dump));
    assert!(reapplied.status.success(), "{}", err(&reapplied));
    assert_eq!(code(&env.cli(Some("curl"), &["init", "--preseed"], Some(&dump))), 9);

    env.ok(None, &["trust", "remove", "ci", "--yes"]);
    std::thread::sleep(Duration::from_secs(6));
    let revoked = env.cli(Some("ci"), &["list"], None);
    assert_eq!(code(&revoked), 9, "{}", err(&revoked));
    assert!(err(&revoked).contains("not trusted"));
}
