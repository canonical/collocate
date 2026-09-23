use collocate_core::spec::{Mount, Spec};
use collocate_core::{Error, Result};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountOp {
    Proc,
    SysfsRo,
    Cgroup2Ro,
    DevTmpfs,
    DevNode(&'static str),
    DevPts,
    DevShm { size: u64 },
    RunTmpfs,
    Tmpfs { dst: String, size: Option<u64> },
    Bind { src: PathBuf, dst: String, ro: bool },
    MaskFile(String),
    MaskDir(String),
    ProcSysRo,
}

#[derive(Debug, Clone)]
pub struct Extras {
    pub resolv: PathBuf,
    pub hosts: PathBuf,
    pub hostname: PathBuf,
    pub init: PathBuf,
    pub secrets_dir: PathBuf,
    pub volumes: HashMap<String, PathBuf>,
}

fn mount_dst(m: &Mount) -> &str {
    match m {
        Mount::Bind { dst, .. }
        | Mount::Tmpfs { dst, .. }
        | Mount::Volume { dst, .. }
        | Mount::Secret { dst, .. }
        | Mount::Config { dst, .. } => dst,
    }
}

const DEV_NODES: [&str; 6] = ["null", "zero", "full", "random", "urandom", "tty"];
const MASKED_FILES: [&str; 3] = ["/proc/kcore", "/proc/keys", "/proc/sysrq-trigger"];
const SHM_SIZE: u64 = 64 << 20;

pub fn mount_plan(spec: &Spec, extras: &Extras) -> Result<Vec<MountOp>> {
    let mut ops = vec![MountOp::Proc, MountOp::DevTmpfs];
    ops.extend(DEV_NODES.iter().map(|n| MountOp::DevNode(n)));
    ops.push(MountOp::DevPts);
    ops.push(MountOp::DevShm { size: SHM_SIZE });
    ops.push(MountOp::SysfsRo);
    ops.push(MountOp::Cgroup2Ro);
    ops.push(MountOp::RunTmpfs);
    if !spec.mounts.iter().any(|m| mount_dst(m) == "/tmp") {
        ops.push(MountOp::Tmpfs { dst: "/tmp".to_string(), size: None });
    }
    for (src, dst) in [
        (&extras.resolv, "/etc/resolv.conf"),
        (&extras.hosts, "/etc/hosts"),
        (&extras.hostname, "/etc/hostname"),
        (&extras.init, "/.collocate/init"),
    ] {
        ops.push(MountOp::Bind { src: src.clone(), dst: dst.to_string(), ro: true });
    }
    for m in &spec.mounts {
        ops.push(match m {
            Mount::Bind { src, dst, ro } => MountOp::Bind { src: src.into(), dst: dst.clone(), ro: *ro },
            Mount::Tmpfs { dst, size } => MountOp::Tmpfs { dst: dst.clone(), size: *size },
            Mount::Volume { name, dst } => {
                let src = extras.volumes.get(name).ok_or_else(|| Error::NotFound(format!("volume {name}")))?;
                MountOp::Bind { src: src.clone(), dst: dst.clone(), ro: false }
            }
            Mount::Secret { name, dst } => MountOp::Bind { src: extras.secrets_dir.join(name), dst: dst.clone(), ro: true },
            Mount::Config { src, dst } => MountOp::Bind { src: src.into(), dst: dst.clone(), ro: true },
        });
    }
    ops.extend(MASKED_FILES.iter().map(|p| MountOp::MaskFile((*p).to_string())));
    ops.push(MountOp::MaskDir("/sys/firmware".to_string()));
    ops.push(MountOp::ProcSysRo);
    Ok(ops)
}
