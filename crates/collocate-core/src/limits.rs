use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_PIDS_MAX: u64 = 4096;

fn default_pids() -> u64 {
    DEFAULT_PIDS_MAX
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ulimit {
    pub name: String,
    pub soft: u64,
    pub hard: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub cpus_milli: Option<u32>,
    #[serde(default)]
    pub cpu_weight: Option<u32>,
    #[serde(default)]
    pub memory: Option<u64>,
    #[serde(default)]
    pub swap: Option<u64>,
    #[serde(default = "default_pids")]
    pub pids_max: u64,
    #[serde(default)]
    pub ulimits: Vec<Ulimit>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { cpus_milli: None, cpu_weight: None, memory: None, swap: None, pids_max: DEFAULT_PIDS_MAX, ulimits: Vec::new() }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<()> {
        if matches!(self.cpu_weight, Some(w) if !(1..=10000).contains(&w)) {
            return Err(Error::InvalidSpec("cpu weight must be within 1..=10000".into()));
        }
        if self.cpus_milli == Some(0) {
            return Err(Error::InvalidSpec("cpus must be positive".into()));
        }
        if self.pids_max == 0 {
            return Err(Error::InvalidSpec("pids max must be positive".into()));
        }
        if self.memory == Some(0) {
            return Err(Error::InvalidSpec("memory must be positive".into()));
        }
        Ok(())
    }

    pub fn cgroup_writes(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(m) = self.cpus_milli {
            out.push(("cpu.max".to_string(), format!("{} 100000", u64::from(m) * 100)));
        }
        if let Some(w) = self.cpu_weight {
            out.push(("cpu.weight".to_string(), w.to_string()));
        }
        if let Some(mem) = self.memory {
            out.push(("memory.max".to_string(), mem.to_string()));
            out.push(("memory.swap.max".to_string(), self.swap.unwrap_or(0).to_string()));
            out.push(("memory.oom.group".to_string(), "1".to_string()));
        } else if let Some(swap) = self.swap {
            out.push(("memory.swap.max".to_string(), swap.to_string()));
        }
        out.push(("pids.max".to_string(), self.pids_max.to_string()));
        out
    }
}
