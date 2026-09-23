#![allow(dead_code)]
use collocate_core::spec::{RootSource, Series, Spec};
use collocate_runtime::launch::{spawn, LaunchRequest, RootMode, Started};
use collocate_runtime::mountplan::Extras;
use collocate_sys::pidfd::{wait, ExitStatus};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn init_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin = root.join("target/x86_64-unknown-linux-musl/debug/collocate-init");
    if !bin.exists() {
        let status = std::process::Command::new("cargo").args(["build", "-p", "collocate-init"]).current_dir(&root).status().unwrap();
        assert!(status.success());
    }
    bin
}

pub struct World {
    pub dir: tempfile::TempDir,
    pub rootfs: PathBuf,
}

const APPLETS: [&str; 16] =
    ["sh", "cat", "ls", "echo", "grep", "id", "hostname", "touch", "wc", "sleep", "env", "tr", "true", "false", "kill", "head"];

impl World {
    pub fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let rootfs = dir.path().join("rootfs");
        for d in ["bin", "etc", "proc", "sys", "dev", "run", "tmp", "data", "root", ".collocate"] {
            fs::create_dir_all(rootfs.join(d)).unwrap();
        }
        for f in ["etc/resolv.conf", "etc/hosts", "etc/hostname", ".collocate/init"] {
            fs::write(rootfs.join(f), "").unwrap();
        }
        fs::write(rootfs.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534::/:/bin/sh\n").unwrap();
        fs::write(rootfs.join("etc/group"), "root:x:0:\nnobody:x:65534:\n").unwrap();
        fs::copy("/usr/bin/busybox", rootfs.join("bin/busybox")).unwrap();
        for a in APPLETS {
            symlink("busybox", rootfs.join("bin").join(a)).unwrap();
        }
        World { dir, rootfs }
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    pub fn spec(&self, name: &str, script: &str) -> Spec {
        let mut s = Spec::new(
            name,
            RootSource::Base { series: Series::Noble, build_id: "t".into() },
            vec!["/bin/sh".into(), "-c".into(), script.into()],
        );
        s.process.env.push(("PATH".into(), "/bin".into()));
        s
    }

    pub fn request<'a>(&'a self, spec: &'a Spec, cgroup_dir: &Path) -> LaunchRequest<'a> {
        let files = self.path(&format!("files-{}", spec.id));
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("resolv.conf"), "nameserver 10.1.2.3\n").unwrap();
        fs::write(files.join("hosts"), "127.0.0.1\tlocalhost\n10.9.9.9\tpeer\n").unwrap();
        fs::write(files.join("hostname"), format!("{}\n", spec.hostname)).unwrap();
        let secrets = self.path(&format!("secrets-{}", spec.id));
        fs::create_dir_all(&secrets).unwrap();
        let staging = self.path(&format!("roots/{}", spec.id));
        fs::create_dir_all(&staging).unwrap();
        LaunchRequest {
            spec,
            root_mode: RootMode::BindRo,
            lowers: vec![self.rootfs.clone()],
            upper: self.path("upper"),
            work: self.path("work"),
            staging,
            extras: Extras {
                resolv: files.join("resolv.conf"),
                hosts: files.join("hosts"),
                hostname: files.join("hostname"),
                init: init_binary(),
                secrets_dir: secrets,
                volumes: HashMap::new(),
            },
            cgroup_dir: cgroup_dir.to_path_buf(),
            log_path: self.path(&format!("{}.log", spec.id)),
            net: None,
        }
    }
}

pub fn wait_exit(started: &Started) -> ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(s) = wait(&started.pidfd, true).unwrap() {
            return s;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "container did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn run_to_completion(world: &World, spec: &Spec, cgroup_dir: &Path) -> (ExitStatus, String) {
    let req = world.request(spec, cgroup_dir);
    let started = spawn(&req).unwrap();
    started.confirm(Duration::from_secs(5)).unwrap();
    let status = wait_exit(&started);
    let log = fs::read_to_string(&req.log_path).unwrap_or_default();
    (status, log)
}

pub struct CgroupSandbox(pub PathBuf);

static SANDBOXES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl CgroupSandbox {
    pub fn new(tag: &str) -> CgroupSandbox {
        let n = SANDBOXES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = PathBuf::from(format!("/sys/fs/cgroup/collocate-launch-{}-{n}-{tag}", std::process::id()));
        fs::create_dir(&p).unwrap();
        CgroupSandbox(p)
    }
}

impl Drop for CgroupSandbox {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("cgroup.kill"), "1");
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(rd) = fs::read_dir(&self.0) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    let _ = fs::remove_dir(e.path());
                }
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}
