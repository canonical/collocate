mod common;

use common::docker_archive_with_busybox;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const CLI: &str = env!("CARGO_BIN_EXE_collocate");
const NOBODY: u32 = 65534;

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn debug_binary(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = root.join("target/x86_64-unknown-linux-musl/debug").join(name);
    if !bin.exists() {
        assert!(Command::new("cargo").args(["build", "-p", name]).current_dir(&root).status().unwrap().success());
    }
    bin
}

struct Snap {
    dir: tempfile::TempDir,
    snap: PathBuf,
    common: PathBuf,
    daemon: Option<Child>,
    bridge: String,
}

impl Snap {
    fn new() -> Snap {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let snap = dir.path().join("snap");
        let common = dir.path().join("common");
        fs::create_dir_all(snap.join("bin")).unwrap();
        fs::create_dir_all(&common).unwrap();
        fs::copy(debug_binary("collocate-init"), snap.join("bin/collocate-init")).unwrap();
        fs::copy(debug_binary("collocated"), snap.join("bin/collocated")).unwrap();
        fs::copy(CLI, snap.join("bin/collocate")).unwrap();
        Snap { dir, snap, common, daemon: None, bridge: format!("colsn{}", std::process::id() % 10000) }
    }

    fn sock(&self) -> PathBuf {
        self.common.join("run/collocate.sock")
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env("SNAP", &self.snap).env("SNAP_COMMON", &self.common).env_remove("SNAP_COOKIE").env_remove("SNAP_CONTEXT");
    }

    fn start(&mut self) {
        let mut cmd = Command::new(self.snap.join("bin/collocated"));
        self.env(&mut cmd);
        let child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().unwrap();
        self.daemon = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.sock().exists() {
            assert!(Instant::now() < deadline, "daemon socket never appeared");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn cli_as(&self, uid: Option<u32>, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(self.snap.join("bin/collocate"));
        self.env(&mut cmd);
        cmd.arg("--host").arg(self.sock()).args(args).current_dir(self.dir.path());
        if let Some(u) = uid {
            cmd.uid(u).gid(u);
        }
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            use std::io::Write;
            pipe.write_all(text.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.cli_as(None, args, None)
    }

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cli(args);
        assert!(o.status.success(), "{args:?} failed: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn info(&self) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["info", "--format", "json"])).unwrap()
    }

    fn daemon_pid(&self) -> u32 {
        self.daemon.as_ref().unwrap().id()
    }

    fn teardown(&mut self) -> Output {
        if let Some(mut c) = self.daemon.take() {
            unsafe {
                libc::kill(c.id() as i32, libc::SIGTERM);
            }
            let _ = c.wait();
        }
        let mut cmd = Command::new(self.snap.join("bin/collocated"));
        self.env(&mut cmd);
        cmd.arg("--teardown").output().unwrap()
    }
}

impl Drop for Snap {
    fn drop(&mut self) {
        let _ = self.teardown();
        let _ = Command::new("ip").args(["link", "del", &self.bridge]).output();
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

#[test]
fn a_fresh_install_waits_for_init_then_runs_containers_and_tears_down_cleanly() {
    if !is_root() {
        eprintln!("skipping: needs root");
        return;
    }
    if Path::new("/sys/fs/cgroup/collocate.slice").exists() {
        eprintln!("skipping: a collocate daemon already owns /sys/fs/cgroup/collocate.slice");
        return;
    }
    let mut s = Snap::new();
    s.start();
    let pid = s.daemon_pid();

    let info = s.info();
    assert_eq!(info["initialized"], false, "{info}");
    assert!(!Path::new("/sys/class/net").join(&s.bridge).exists());
    let refused = s.cli(&["list"]);
    assert_eq!(code(&refused), 7, "{}", String::from_utf8_lossy(&refused.stderr));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("collocate init"));
    let piped = s.cli(&["init"]);
    assert_eq!(code(&piped), 2);
    assert!(String::from_utf8_lossy(&piped.stderr).contains("--auto"));

    fs::set_permissions(s.sock(), fs::Permissions::from_mode(0o666)).unwrap();
    let denied = s.cli_as(Some(NOBODY), &["init", "--auto", "--bridge", &s.bridge], None);
    assert_eq!(code(&denied), 8, "{}", String::from_utf8_lossy(&denied.stderr));

    let bridge = s.bridge.clone();
    s.ok(&["init", "--auto", "--subnet", "10.215.0.0/24", "--bridge", &bridge, "--root-mode", "bind-ro", "--node-name", "snaptest"]);
    let info = s.info();
    assert_eq!(info["initialized"], true, "{info}");
    assert_eq!(info["subnet"], "10.215.0.0/24");
    assert_eq!(info["node"], "snaptest");
    assert_eq!(s.daemon_pid(), pid);
    assert!(s.daemon.as_mut().unwrap().try_wait().unwrap().is_none(), "the daemon re-executed in place");
    assert!(Path::new("/sys/class/net").join(&s.bridge).exists());
    let config = fs::read_to_string(s.common.join("collocated.toml")).unwrap();
    assert!(!config.contains("init_path") && !config.contains(s.snap.to_str().unwrap()), "{config}");
    let staged: Vec<_> = fs::read_dir(s.common.join("state/bin")).unwrap().flatten().collect();
    assert_eq!(staged.len(), 1);

    let dump = s.ok(&["init", "--dump"]);
    assert!(dump.contains("10.215.0.0/24") && dump.contains("snaptest"), "{dump}");

    assert_eq!(fs::metadata(s.sock()).unwrap().permissions().mode() & 0o777, 0o660);
    fs::set_permissions(s.sock(), fs::Permissions::from_mode(0o666)).unwrap();
    let public = tempfile::tempdir().unwrap();
    fs::set_permissions(public.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let archive = public.path().join("app.tar");
    fs::write(&archive, docker_archive_with_busybox("snapapp:1", &["/bin/sh", "-c", "echo from-image=$FROM_IMAGE"])).unwrap();
    std::os::unix::fs::chown(&archive, Some(NOBODY), Some(NOBODY)).unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o600)).unwrap();
    let imported = s.cli_as(Some(NOBODY), &["image", "import", archive.to_str().unwrap()], None);
    assert!(imported.status.success(), "non-root import failed: {}", String::from_utf8_lossy(&imported.stderr));
    let by_root = s.cli(&["image", "import", archive.to_str().unwrap()]);
    assert!(by_root.status.success(), "{}", String::from_utf8_lossy(&by_root.stderr));
    let listed = s.cli_as(Some(NOBODY), &["image", "list"], None);
    assert!(String::from_utf8_lossy(&listed.stdout).contains("snapapp:1"));
    let unreadable = s.cli_as(Some(NOBODY), &["image", "import", "/root/definitely-not-readable.tar"], None);
    assert_ne!(code(&unreadable), 0);

    {
        use collocate_core::request::{Request, Response};
        use collocate_sys::fdpass::SendWithFds;
        use std::os::fd::AsRawFd;
        let mut c = collocate_core::client::Client::connect(s.sock()).unwrap();
        for _ in 0..2 {
            let f = fs::File::open(&archive).unwrap();
            c.send_with_fds(&Request::ImageImport, &[&f.as_raw_fd()]).unwrap();
            assert!(matches!(c.read_response().unwrap(), Response::Json { .. }));
        }
        assert!(matches!(c.call(&Request::ImageList).unwrap(), Response::Json { .. }), "the connection survives worker requests");
        assert!(matches!(c.call(&Request::Ps { all: true, project: None }).unwrap(), Response::Containers(_)));
    }

    let out = s.ok(&["run", "--image", "snapapp:1", "--name", "fromsnap"]);
    assert!(out.contains("from-image=yes"), "{out}");
    s.ok(&["run", "-d", "--image", "snapapp:1", "--name", "sleeper", "--entrypoint", "/bin/sleep", "--", "300"]);

    let conflict = s.cli(&["init", "--auto", "--subnet", "10.216.0.0/24", "--bridge", &bridge, "--root-mode", "bind-ro"]);
    assert_eq!(code(&conflict), 5, "{}", String::from_utf8_lossy(&conflict.stderr));
    s.ok(&["init", "--auto", "--force", "--subnet", "10.216.0.0/24", "--bridge", &bridge, "--root-mode", "bind-ro"]);
    assert_eq!(s.info()["subnet"], "10.216.0.0/24");
    let left = s.ok(&["list", "-a", "--format", "json"]);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&left).unwrap().as_array().unwrap().len(), 0, "{left}");
    s.ok(&["run", "-d", "--image", "snapapp:1", "--name", "survivor", "--entrypoint", "/bin/sleep", "--", "300"]);

    let t = s.teardown();
    assert!(t.status.success(), "{}", String::from_utf8_lossy(&t.stderr));
    assert!(!Path::new("/sys/fs/cgroup/collocate.slice").exists());
    assert!(!Path::new("/sys/class/net").join(&s.bridge).exists());
    let tables = Command::new("nft").args(["list", "tables"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&tables.stdout).contains("inet collocate"));
}
