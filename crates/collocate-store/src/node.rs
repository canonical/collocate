use serde::{Deserialize, Serialize};

pub const NODE_SCHEMA: u32 = 1;

pub fn instance_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .or_else(|_| std::fs::read_to_string("/proc/sys/kernel/hostname"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMeta {
    pub schema: u32,
    pub node_uuid: String,
    pub name: String,
    pub subnet: String,
    pub instance_id: String,
}
