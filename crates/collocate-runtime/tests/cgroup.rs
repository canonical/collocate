use collocate_core::limits::Limits;
use collocate_core::ContainerId;
use collocate_runtime::cgroup::CgroupTree;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn id(b: u8) -> ContainerId {
    ContainerId::from_bytes([b; 6])
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

struct Sandbox(PathBuf);

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Sandbox {
    fn new() -> Sandbox {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = PathBuf::from(format!("/sys/fs/cgroup/collocate-rt-test-{}-{n}", std::process::id()));
        fs::create_dir(&p).unwrap();
        fs::write(p.join("cgroup.subtree_control"), "+cpu +memory +pids").unwrap();
        Sandbox(p)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("cgroup.kill"), "1");
        for sub in ["collocate.slice/containers", "collocate.slice/supervisor", "collocate.slice"] {
            if let Ok(rd) = fs::read_dir(self.0.join(sub)) {
                for e in rd.flatten() {
                    if e.path().is_dir() {
                        let _ = fs::remove_dir(e.path());
                    }
                }
            }
            let _ = fs::remove_dir(self.0.join(sub));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn paths_are_derived_from_root_slice_and_id() {
    let t = CgroupTree::new("/sys/fs/cgroup", "collocate.slice");
    assert_eq!(t.container_dir(&id(0xab)), PathBuf::from("/sys/fs/cgroup/collocate.slice/containers/abababababab"));
    assert_eq!(t.supervisor_dir(), PathBuf::from("/sys/fs/cgroup/collocate.slice/supervisor"));
}

#[test]
fn limits_are_written_and_lifecycle_works() {
    if !is_root() {
        return;
    }
    let sb = Sandbox::new();
    let tree = CgroupTree::new(&sb.0, "collocate.slice");
    tree.setup(false).unwrap();
    let controllers = fs::read_to_string(tree.containers_dir().join("cgroup.subtree_control")).unwrap();
    for c in ["cpu", "memory", "pids"] {
        assert!(controllers.split_whitespace().any(|x| x == c), "{c} not delegated: {controllers}");
    }

    let limits = Limits { cpus_milli: Some(500), memory: Some(64 << 20), pids_max: 100, ..Limits::default() };
    let dir = tree.create(&id(1), &limits).unwrap();
    assert_eq!(fs::read_to_string(dir.join("memory.max")).unwrap().trim(), "67108864");
    assert_eq!(fs::read_to_string(dir.join("cpu.max")).unwrap().trim(), "50000 100000");
    assert_eq!(fs::read_to_string(dir.join("pids.max")).unwrap().trim(), "100");
    assert_eq!(fs::read_to_string(dir.join("memory.swap.max")).unwrap().trim(), "0");
    assert_eq!(fs::read_to_string(dir.join("memory.oom.group")).unwrap().trim(), "1");

    assert_eq!(tree.populated(&id(1)), Some(false));
    assert_eq!(tree.populated(&id(2)), None);
    assert_eq!(tree.list().unwrap(), vec![id(1)]);

    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    fs::write(dir.join("cgroup.procs"), child.id().to_string()).unwrap();
    assert_eq!(tree.populated(&id(1)), Some(true));
    assert_eq!(tree.procs(&id(1)).unwrap(), vec![child.id()]);
    let stats = tree.stats(&id(1)).unwrap();
    assert!(stats.memory_current < (64 << 20));
    assert_eq!(stats.memory_max, Some(64 << 20));
    assert_eq!(stats.pids_current, 1);

    tree.kill(&id(1)).unwrap();
    child.wait().unwrap();
    wait_until("cgroup to empty", || tree.populated(&id(1)) == Some(false));
    tree.remove(&id(1)).unwrap();
    assert_eq!(tree.populated(&id(1)), None);
    assert!(tree.list().unwrap().is_empty());
}

#[test]
fn removing_a_missing_cgroup_is_not_an_error() {
    if !is_root() {
        return;
    }
    let sb = Sandbox::new();
    let tree = CgroupTree::new(&sb.0, "collocate.slice");
    tree.setup(false).unwrap();
    tree.remove(&id(9)).unwrap();
}
