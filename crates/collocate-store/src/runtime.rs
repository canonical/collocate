use collocate_core::request::HealthState;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runtime {
    pub pid: u32,
    pub starttime: u64,
    pub cgroup: String,
    pub address: Option<Ipv4Addr>,
    pub health: Option<HealthState>,
}
