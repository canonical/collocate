use crate::ipam::Subnet;
use collocate_core::net::veth_names;
use collocate_core::{ContainerId, Error, Result};
use std::net::Ipv4Addr;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd(pub Vec<String>);

fn cmd(parts: &[&str]) -> Cmd {
    Cmd(parts.iter().map(|s| (*s).to_string()).collect())
}

impl Cmd {
    pub fn run(&self) -> Result<()> {
        self.run_allowing(None)
    }

    pub fn run_allowing(&self, tolerated: Option<&str>) -> Result<()> {
        let (prog, args) = self.0.split_first().ok_or_else(|| Error::Internal("empty command".into()))?;
        let out = Command::new(prog).args(args).output()?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if tolerated.is_some_and(|t| stderr.contains(t)) {
            return Ok(());
        }
        Err(Error::Internal(format!("{} failed: {stderr}", self.0.join(" "))))
    }
}

pub fn bridge_setup(bridge: &str, subnet: &Subnet) -> Vec<Cmd> {
    let cidr = format!("{}/{}", subnet.gateway(), subnet.prefix());
    vec![
        cmd(&["ip", "link", "add", "name", bridge, "type", "bridge"]),
        cmd(&["ip", "addr", "replace", &cidr, "dev", bridge]),
        cmd(&["ip", "link", "set", bridge, "up"]),
        cmd(&["sysctl", "-qw", "net.ipv4.ip_forward=1"]),
    ]
}

pub fn veth_setup(id: &ContainerId, bridge: &str) -> Vec<Cmd> {
    let (host, peer) = veth_names(id);
    vec![
        cmd(&["ip", "link", "add", &host, "type", "veth", "peer", "name", &peer]),
        cmd(&["ip", "link", "set", &host, "master", bridge]),
        cmd(&["ip", "link", "set", &host, "up"]),
    ]
}

pub fn move_to_ns(id: &ContainerId, pid: i32) -> Cmd {
    let (_, peer) = veth_names(id);
    cmd(&["ip", "link", "set", &peer, "netns", &pid.to_string()])
}

pub fn in_ns_config(pid: i32, id: &ContainerId, addr: Ipv4Addr, prefix: u8, gateway: Ipv4Addr, ping_group: bool) -> Vec<Cmd> {
    let (_, peer) = veth_names(id);
    let pid = pid.to_string();
    let inner = |parts: &[&str]| {
        let mut v = vec!["nsenter".to_string(), "-t".into(), pid.clone(), "-n".into(), "--".into()];
        v.extend(parts.iter().map(|s| (*s).to_string()));
        Cmd(v)
    };
    let mut cmds = vec![
        inner(&["ip", "link", "set", "lo", "up"]),
        inner(&["ip", "link", "set", &peer, "name", "eth0"]),
        inner(&["ip", "addr", "add", &format!("{addr}/{prefix}"), "dev", "eth0"]),
        inner(&["ip", "link", "set", "eth0", "up"]),
        inner(&["ip", "route", "add", "default", "via", &gateway.to_string()]),
    ];
    if ping_group {
        cmds.push(inner(&["sysctl", "-qw", "net.ipv4.ping_group_range=0 2147483647"]));
    }
    cmds
}

pub fn teardown_veth(id: &ContainerId) -> Cmd {
    let (host, _) = veth_names(id);
    cmd(&["ip", "link", "del", &host])
}
