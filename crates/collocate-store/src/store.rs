use crate::fsutil::{atomic_write, sync_dir};
use crate::node::{NodeMeta, NODE_SCHEMA};
use crate::runtime::Runtime;
use collocate_core::id::random_hex;
use collocate_core::spec::{Spec, SPEC_SCHEMA};
use collocate_core::{ContainerId, Error, Result};
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub struct NodeStore {
    state: PathBuf,
    run: PathBuf,
}

pub struct StoreLock {
    _file: File,
}

impl NodeStore {
    pub fn open(state: impl Into<PathBuf>, run: impl Into<PathBuf>) -> Result<Self> {
        let store = NodeStore { state: state.into(), run: run.into() };
        for d in [store.state_containers(), store.run_containers()] {
            fs::create_dir_all(d)?;
        }
        Ok(store)
    }

    fn state_containers(&self) -> PathBuf {
        self.state.join("containers")
    }

    fn run_containers(&self) -> PathBuf {
        self.run.join("containers")
    }

    fn trash(&self) -> PathBuf {
        self.state.join(".trash")
    }

    fn corrupt(&self) -> PathBuf {
        self.state.join(".corrupt")
    }

    fn node_file(&self) -> PathBuf {
        self.state.join("node.json")
    }

    pub fn persistent_dir(&self, id: &ContainerId) -> PathBuf {
        self.state_containers().join(id.to_string())
    }

    pub fn runtime_dir(&self, id: &ContainerId) -> PathBuf {
        self.run_containers().join(id.to_string())
    }

    pub fn lock(&self) -> Result<StoreLock> {
        fs::create_dir_all(&self.state)?;
        let file = File::options().create(true).truncate(false).write(true).open(self.state.join(".lock"))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            return if err.kind() == std::io::ErrorKind::WouldBlock {
                Err(Error::Conflict("another collocated instance holds the node lock".into()))
            } else {
                Err(err.into())
            };
        }
        Ok(StoreLock { _file: file })
    }

    fn read_node(&self) -> Result<Option<NodeMeta>> {
        match fs::read(self.node_file()) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write_node(&self, meta: &NodeMeta) -> Result<()> {
        fs::create_dir_all(&self.state)?;
        atomic_write(&self.node_file(), &serde_json::to_vec_pretty(meta)?)
    }

    pub fn open_node(&self, name: &str, subnet: &str, instance_id: &str) -> Result<NodeMeta> {
        match self.read_node()? {
            None => {
                let meta = NodeMeta {
                    schema: NODE_SCHEMA,
                    node_uuid: random_hex(16)?,
                    name: name.to_string(),
                    subnet: subnet.to_string(),
                    instance_id: instance_id.to_string(),
                };
                self.write_node(&meta)?;
                Ok(meta)
            }
            Some(meta) => {
                if meta.schema > NODE_SCHEMA {
                    return Err(Error::Invalid("node metadata is from a newer version".into()));
                }
                if meta.subnet != subnet {
                    return Err(Error::Conflict(format!("node subnet is {} but {subnet} was requested", meta.subnet)));
                }
                if meta.instance_id != instance_id {
                    return Err(Error::Conflict("node identity changed; use adopt or reinit".into()));
                }
                Ok(meta)
            }
        }
    }

    pub fn adopt_node(&self, instance_id: &str) -> Result<NodeMeta> {
        let mut meta = self.read_node()?.ok_or_else(|| Error::NotFound("node metadata".into()))?;
        meta.instance_id = instance_id.to_string();
        self.write_node(&meta)?;
        Ok(meta)
    }

    pub fn reinit_node(&self, instance_id: &str) -> Result<NodeMeta> {
        let mut meta = self.read_node()?.ok_or_else(|| Error::NotFound("node metadata".into()))?;
        for spec in self.load_all()? {
            self.remove(&spec.id)?;
        }
        self.purge_trash()?;
        meta.node_uuid = random_hex(16)?;
        meta.instance_id = instance_id.to_string();
        self.write_node(&meta)?;
        Ok(meta)
    }

    fn spec_path(&self, id: &ContainerId) -> Option<PathBuf> {
        [self.persistent_dir(id), self.runtime_dir(id)].into_iter().map(|d| d.join("spec.json")).find(|p| p.exists())
    }

    pub fn create(&self, spec: &Spec) -> Result<()> {
        if self.spec_path(&spec.id).is_some() {
            return Err(Error::Conflict(format!("container {} already exists", spec.id)));
        }
        if !spec.name.is_empty() && self.load_all()?.iter().any(|s| s.name == spec.name) {
            return Err(Error::Conflict(format!("name {} is already in use", spec.name)));
        }
        let base = if spec.persistent { self.state_containers() } else { self.run_containers() };
        fs::create_dir_all(&base)?;
        let tmp = base.join(format!(".tmp-{}", spec.id));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;
        atomic_write(&tmp.join("spec.json"), &serde_json::to_vec_pretty(spec)?)?;
        let final_dir = base.join(spec.id.to_string());
        if final_dir.exists() {
            fs::remove_dir_all(&tmp)?;
            if final_dir.join("spec.json").exists() {
                return Err(Error::Conflict(format!("container {} already exists", spec.id)));
            }
            fs::remove_dir(&final_dir)?;
            fs::create_dir_all(&tmp)?;
            atomic_write(&tmp.join("spec.json"), &serde_json::to_vec_pretty(spec)?)?;
        }
        fs::rename(&tmp, &final_dir)?;
        sync_dir(&base)
    }

    pub fn update(&self, spec: &Spec) -> Result<()> {
        let path = self.spec_path(&spec.id).ok_or_else(|| Error::NotFound(spec.id.to_string()))?;
        atomic_write(&path, &serde_json::to_vec_pretty(spec)?)
    }

    pub fn get(&self, id: &ContainerId) -> Result<Spec> {
        let path = self.spec_path(id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    pub fn load_all(&self) -> Result<Vec<Spec>> {
        let mut out = Vec::new();
        for base in [self.state_containers(), self.run_containers()] {
            let Ok(rd) = fs::read_dir(&base) else { continue };
            for entry in rd {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let spec_file = entry.path().join("spec.json");
                let bytes = match fs::read(&spec_file) {
                    Ok(b) => b,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                match serde_json::from_slice::<Spec>(&bytes) {
                    Ok(spec) if spec.schema > SPEC_SCHEMA => {
                        return Err(Error::Invalid(format!("spec {name} uses newer schema {}", spec.schema)));
                    }
                    Ok(spec) => out.push(spec),
                    Err(_) => self.quarantine(&entry.path(), &name)?,
                }
            }
        }
        out.sort_by_key(|s| (s.created, s.id));
        Ok(out)
    }

    fn quarantine(&self, dir: &Path, name: &str) -> Result<()> {
        fs::create_dir_all(self.corrupt())?;
        let dest = self.corrupt().join(name);
        let _ = fs::remove_dir_all(&dest);
        fs::rename(dir, dest)?;
        Ok(())
    }

    pub fn remove(&self, id: &ContainerId) -> Result<()> {
        let persistent = self.persistent_dir(id);
        let ephemeral = self.runtime_dir(id);
        if !persistent.join("spec.json").exists() && !ephemeral.join("spec.json").exists() {
            return Err(Error::NotFound(id.to_string()));
        }
        if persistent.exists() {
            fs::create_dir_all(self.trash())?;
            fs::rename(&persistent, self.trash().join(id.to_string()))?;
        }
        if ephemeral.exists() {
            fs::remove_dir_all(&ephemeral)?;
        }
        Ok(())
    }

    pub fn purge_trash(&self) -> Result<()> {
        if let Ok(rd) = fs::read_dir(self.trash()) {
            for e in rd {
                fs::remove_dir_all(e?.path())?;
            }
        }
        Ok(())
    }

    pub fn recover(&self) -> Result<()> {
        for base in [self.state_containers(), self.run_containers()] {
            let Ok(rd) = fs::read_dir(&base) else { continue };
            for e in rd {
                let e = e?;
                if e.file_name().to_string_lossy().starts_with(".tmp-") {
                    fs::remove_dir_all(e.path())?;
                }
            }
        }
        self.purge_trash()
    }

    pub fn write_runtime(&self, id: &ContainerId, rt: &Runtime) -> Result<()> {
        let dir = self.runtime_dir(id);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("runtime.json"), &serde_json::to_vec(rt)?)
    }

    pub fn read_runtime(&self, id: &ContainerId) -> Result<Option<Runtime>> {
        match fs::read(self.runtime_dir(id).join("runtime.json")) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn clear_runtime(&self, id: &ContainerId) -> Result<()> {
        match fs::remove_file(self.runtime_dir(id).join("runtime.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
