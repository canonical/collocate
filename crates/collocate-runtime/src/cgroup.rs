use collocate_core::limits::Limits;
use collocate_core::procinfo::{parse_cgroup_events, parse_cpu_stat};
use collocate_core::ContainerId;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    pub cpu_usage_usec: u64,
    pub memory_current: u64,
    pub memory_max: Option<u64>,
    pub pids_current: u64,
}

#[derive(Debug, Clone)]
pub struct CgroupTree {
    root: PathBuf,
    slice: String,
}

const CONTROLLERS: &str = "+cpu +memory +pids";

fn read_u64(path: &Path) -> io::Result<u64> {
    fs::read_to_string(path)?.trim().parse().map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

impl CgroupTree {
    pub fn new(root: impl AsRef<Path>, slice: &str) -> Self {
        CgroupTree { root: root.as_ref().to_path_buf(), slice: slice.to_string() }
    }

    pub fn slice_dir(&self) -> PathBuf {
        self.root.join(&self.slice)
    }

    pub fn supervisor_dir(&self) -> PathBuf {
        self.slice_dir().join("supervisor")
    }

    pub fn containers_dir(&self) -> PathBuf {
        self.slice_dir().join("containers")
    }

    pub fn container_dir(&self, id: &ContainerId) -> PathBuf {
        self.containers_dir().join(id.to_string())
    }

    pub fn setup(&self, move_self: bool) -> io::Result<()> {
        fs::write(self.root.join("cgroup.subtree_control"), CONTROLLERS)?;
        fs::create_dir_all(self.supervisor_dir())?;
        fs::create_dir_all(self.containers_dir())?;
        if move_self {
            fs::write(self.supervisor_dir().join("cgroup.procs"), std::process::id().to_string())?;
        }
        fs::write(self.slice_dir().join("cgroup.subtree_control"), CONTROLLERS)?;
        fs::write(self.containers_dir().join("cgroup.subtree_control"), CONTROLLERS)?;
        Ok(())
    }

    pub fn create(&self, id: &ContainerId, limits: &Limits) -> io::Result<PathBuf> {
        let dir = self.container_dir(id);
        fs::create_dir(&dir)?;
        for (name, value) in limits.cgroup_writes() {
            fs::write(dir.join(name), value)?;
        }
        Ok(dir)
    }

    pub fn kill(&self, id: &ContainerId) -> io::Result<()> {
        match fs::write(self.container_dir(id).join("cgroup.kill"), "1") {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    pub fn remove(&self, id: &ContainerId) -> io::Result<()> {
        let dir = self.container_dir(id);
        for _ in 0..100 {
            match fs::remove_dir(&dir) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) if e.raw_os_error() == Some(libc::EBUSY) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(e),
            }
        }
        fs::remove_dir(&dir)
    }

    pub fn populated(&self, id: &ContainerId) -> Option<bool> {
        let text = fs::read_to_string(self.container_dir(id).join("cgroup.events")).ok()?;
        parse_cgroup_events(&text).ok().map(|e| e.populated)
    }

    pub fn procs(&self, id: &ContainerId) -> io::Result<Vec<u32>> {
        let text = fs::read_to_string(self.container_dir(id).join("cgroup.procs"))?;
        Ok(text.lines().filter_map(|l| l.trim().parse().ok()).collect())
    }

    pub fn stats(&self, id: &ContainerId) -> io::Result<Stats> {
        let dir = self.container_dir(id);
        let cpu = parse_cpu_stat(&fs::read_to_string(dir.join("cpu.stat"))?).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let max = fs::read_to_string(dir.join("memory.max")).ok().and_then(|t| t.trim().parse().ok());
        Ok(Stats {
            cpu_usage_usec: cpu.usage_usec,
            memory_current: read_u64(&dir.join("memory.current"))?,
            memory_max: max,
            pids_current: read_u64(&dir.join("pids.current"))?,
        })
    }

    pub fn list(&self) -> io::Result<Vec<ContainerId>> {
        let mut ids = Vec::new();
        match fs::read_dir(self.containers_dir()) {
            Ok(rd) => {
                for e in rd {
                    let e = e?;
                    if let Some(id) = e.file_name().to_str().and_then(|n| ContainerId::parse(n).ok()) {
                        ids.push(id);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        ids.sort();
        Ok(ids)
    }
}
