use collocate_core::net::{Proto, Publish};
use collocate_core::ContainerId;
use collocate_net::ipam::Subnet;
use collocate_net::ruleset::{render, Algorithm, Backend, ContainerPorts, LbRule, NoBackends, Ruleset};
use std::net::Ipv4Addr;

fn ip(s: &str) -> Ipv4Addr {
    s.parse().unwrap()
}

fn base() -> Ruleset {
    Ruleset { bridge: "collocate0".into(), subnet: Subnet::parse("172.30.0.0/16").unwrap(), containers: vec![], lbs: vec![] }
}

fn cid(b: u8) -> ContainerId {
    ContainerId::from_bytes([b; 6])
}

#[test]
fn empty_ruleset_replaces_the_table_atomically_with_masquerade() {
    let out = render(&base());
    let add = out.find("add table inet collocate").unwrap();
    let del = out.find("delete table inet collocate").unwrap();
    let def = out.find("table inet collocate {").unwrap();
    assert!(add < del && del < def);
    assert!(out.contains("ip saddr 172.30.0.0/16 oifname != \"collocate0\" masquerade"));
    assert!(out.contains("iifname \"collocate0\" accept"));
    assert!(out.contains("ct state established,related accept"));
}

#[test]
fn publish_rules_dnat_external_and_host_traffic_with_tags() {
    let mut r = base();
    r.containers.push(ContainerPorts {
        id: cid(0xab),
        addr: ip("172.30.0.2"),
        publish: vec![Publish::parse("8080:80").unwrap(), Publish::parse("5353:53/udp").unwrap()],
    });
    let out = render(&r);
    assert!(out.contains(
        "iifname != \"collocate0\" fib daddr type local tcp dport 8080 dnat ip to 172.30.0.2:80 comment \"collocate:abababababab\""
    ));
    assert!(out.contains("ip daddr != 127.0.0.0/8 fib daddr type local tcp dport 8080 dnat ip to 172.30.0.2:80"));
    assert!(out.contains("udp dport 5353 dnat ip to 172.30.0.2:53"));
}

#[test]
fn containers_without_publishes_add_no_dnat() {
    let mut r = base();
    r.containers.push(ContainerPorts { id: cid(1), addr: ip("172.30.0.2"), publish: vec![] });
    assert!(!render(&r).contains("dnat"));
}

fn lb(alg: Algorithm, backends: Vec<Backend>) -> LbRule {
    LbRule {
        name: "app/web".into(),
        vip: ip("172.30.255.1"),
        proto: Proto::Tcp,
        listen: 80,
        publish: vec![8080],
        algorithm: alg,
        backends,
        on_no_backends: NoBackends::Reject,
    }
}

fn be(addr: &str, port: u16, weight: u32) -> Backend {
    Backend { addr: ip(addr), port, weight }
}

#[test]
fn round_robin_uses_numgen_inc_over_a_map() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::RoundRobin, vec![be("172.30.0.4", 8080, 1), be("172.30.0.5", 8080, 1)]));
    let out = render(&r);
    assert!(out.contains(
        "ip daddr 172.30.255.1 tcp dport 80 dnat ip addr . port to numgen inc mod 2 map { 0 : 172.30.0.4 . 8080, 1 : 172.30.0.5 . 8080 }"
    ));
    assert!(out.contains("comment \"collocate-lb:app/web\""));
}

#[test]
fn random_and_source_hash_algorithms() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::Random, vec![be("172.30.0.4", 8080, 1), be("172.30.0.5", 8080, 1)]));
    assert!(render(&r).contains("numgen random mod 2 map"));
    let mut r = base();
    r.lbs.push(lb(Algorithm::SourceHash, vec![be("172.30.0.4", 8080, 1), be("172.30.0.5", 8080, 1), be("172.30.0.6", 8080, 1)]));
    assert!(render(&r).contains("jhash ip saddr mod 3 map"));
}

#[test]
fn weights_repeat_entries() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::RoundRobin, vec![be("172.30.0.4", 8080, 2), be("172.30.0.5", 8080, 1)]));
    let out = render(&r);
    assert!(out.contains("numgen inc mod 3 map { 0 : 172.30.0.4 . 8080, 1 : 172.30.0.4 . 8080, 2 : 172.30.0.5 . 8080 }"));
}

#[test]
fn external_publish_dnats_the_host_port_through_the_same_map() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::RoundRobin, vec![be("172.30.0.4", 8080, 1)]));
    let out = render(&r);
    assert!(out.contains("iifname != \"collocate0\" fib daddr type local tcp dport 8080 dnat ip addr . port to numgen inc mod 1 map"));
}

#[test]
fn hairpin_masquerade_covers_lb_backends() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::RoundRobin, vec![be("172.30.0.4", 8080, 1), be("172.30.0.5", 9090, 1)]));
    let out = render(&r);
    assert!(out.contains("ip saddr 172.30.0.0/16 ip daddr { 172.30.0.4, 172.30.0.5 } oifname \"collocate0\" masquerade"));
}

#[test]
fn no_backends_rejects_or_drops_on_the_vip() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::RoundRobin, vec![]));
    let out = render(&r);
    assert!(out.contains("ip daddr 172.30.255.1 tcp dport 80 reject with tcp reset"));
    assert!(!out.contains("numgen"));
    let mut r = base();
    let mut l = lb(Algorithm::RoundRobin, vec![]);
    l.on_no_backends = NoBackends::Drop;
    r.lbs.push(l);
    assert!(render(&r).contains("ip daddr 172.30.255.1 tcp dport 80 drop"));
}

#[test]
fn rendering_is_deterministic() {
    let mut r = base();
    r.lbs.push(lb(Algorithm::RoundRobin, vec![be("172.30.0.4", 8080, 1)]));
    assert_eq!(render(&r), render(&r));
}
