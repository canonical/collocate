use crate::ipam::Subnet;
use collocate_core::net::{nft_tag, Proto, Publish};
pub use collocate_core::net::{Algorithm, NoBackends};
use collocate_core::ContainerId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt::Write;
use std::net::Ipv4Addr;

const MAX_WEIGHT: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerPorts {
    pub id: ContainerId,
    pub addr: Ipv4Addr,
    pub publish: Vec<Publish>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backend {
    pub addr: Ipv4Addr,
    pub port: u16,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LbRule {
    pub name: String,
    pub vip: Ipv4Addr,
    pub proto: Proto,
    pub listen: u16,
    pub publish: Vec<u16>,
    pub algorithm: Algorithm,
    pub backends: Vec<Backend>,
    pub on_no_backends: NoBackends,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ruleset {
    pub bridge: String,
    pub subnet: Subnet,
    pub containers: Vec<ContainerPorts>,
    pub lbs: Vec<LbRule>,
}

impl Serialize for Subnet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Subnet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Subnet::parse(&s).map_err(serde::de::Error::custom)
    }
}

fn expand(backends: &[Backend]) -> Vec<Backend> {
    let mut out = Vec::new();
    for b in backends {
        for _ in 0..b.weight.min(MAX_WEIGHT) {
            out.push(*b);
        }
    }
    out
}

fn dnat_expr(lb: &LbRule, entries: &[Backend]) -> String {
    let n = entries.len();
    let generator = match lb.algorithm {
        Algorithm::RoundRobin => format!("numgen inc mod {n}"),
        Algorithm::Random => format!("numgen random mod {n}"),
        Algorithm::SourceHash => format!("jhash ip saddr mod {n}"),
    };
    let map: Vec<String> = entries.iter().enumerate().map(|(i, b)| format!("{i} : {} . {}", b.addr, b.port)).collect();
    format!("dnat ip addr . port to {generator} map {{ {} }}", map.join(", "))
}

pub fn render(rs: &Ruleset) -> String {
    let br = &rs.bridge;
    let mut pre = String::new();
    let mut out_chain = String::new();
    let mut post = String::new();
    let mut input = String::new();

    for c in &rs.containers {
        let tag = nft_tag(&c.id);
        for p in &c.publish {
            let target = format!("dnat ip to {}:{}", c.addr, p.container);
            let _ = writeln!(pre, "    iifname != \"{br}\" fib daddr type local {} dport {} {target} comment \"{tag}\"", p.proto, p.host);
            let _ = writeln!(out_chain, "    fib daddr type local {} dport {} {target} comment \"{tag}\"", p.proto, p.host);
        }
    }

    for lb in &rs.lbs {
        let entries = expand(&lb.backends);
        let tag = format!("collocate-lb:{}", lb.name);
        if entries.is_empty() {
            let verdict = match (lb.on_no_backends, lb.proto) {
                (NoBackends::Drop, _) => "drop".to_string(),
                (NoBackends::Reject, Proto::Tcp) => "reject with tcp reset".to_string(),
                (NoBackends::Reject, Proto::Udp) => "reject with icmp type port-unreachable".to_string(),
            };
            let _ = writeln!(input, "    ip daddr {} {} dport {} {verdict} comment \"{tag}\"", lb.vip, lb.proto, lb.listen);
            continue;
        }
        let expr = dnat_expr(lb, &entries);
        let _ = writeln!(pre, "    ip daddr {} {} dport {} {expr} comment \"{tag}\"", lb.vip, lb.proto, lb.listen);
        let _ = writeln!(out_chain, "    ip daddr {} {} dport {} {expr} comment \"{tag}\"", lb.vip, lb.proto, lb.listen);
        for port in &lb.publish {
            let _ = writeln!(pre, "    iifname != \"{br}\" fib daddr type local {} dport {port} {expr} comment \"{tag}\"", lb.proto);
            let _ = writeln!(out_chain, "    fib daddr type local {} dport {port} {expr} comment \"{tag}\"", lb.proto);
        }
        let addrs: BTreeSet<Ipv4Addr> = lb.backends.iter().filter(|b| b.weight > 0).map(|b| b.addr).collect();
        let list: Vec<String> = addrs.iter().map(ToString::to_string).collect();
        let _ =
            writeln!(post, "    ip saddr {} ip daddr {{ {} }} oifname \"{br}\" masquerade comment \"{tag}\"", rs.subnet, list.join(", "));
    }

    let mut s = String::new();
    let _ = writeln!(s, "add table inet collocate");
    let _ = writeln!(s, "delete table inet collocate");
    let _ = writeln!(s, "table inet collocate {{");
    let _ = writeln!(s, "  chain prerouting {{\n    type nat hook prerouting priority dstnat; policy accept;\n{pre}  }}");
    let _ = writeln!(s, "  chain output {{\n    type nat hook output priority -100; policy accept;\n{out_chain}  }}");
    let _ = writeln!(
        s,
        "  chain postrouting {{\n    type nat hook postrouting priority srcnat; policy accept;\n    ip saddr {} oifname != \"{br}\" masquerade\n    ip saddr 127.0.0.0/8 oifname \"{br}\" masquerade\n{post}  }}",
        rs.subnet
    );
    let _ = writeln!(
        s,
        "  chain forward {{\n    type filter hook forward priority filter; policy accept;\n    ct state established,related accept\n    iifname \"{br}\" accept\n    oifname \"{br}\" accept\n  }}"
    );
    let _ = writeln!(s, "  chain input {{\n    type filter hook input priority filter; policy accept;\n{input}  }}");
    let _ = writeln!(s, "}}");
    s
}
