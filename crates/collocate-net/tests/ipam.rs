use collocate_core::Error;
use collocate_net::ipam::{Ipam, Subnet};
use std::net::Ipv4Addr;

fn ip(s: &str) -> Ipv4Addr {
    s.parse().unwrap()
}

#[test]
fn subnet_parses_and_exposes_gateway() {
    let s = Subnet::parse("172.30.0.0/16").unwrap();
    assert_eq!(s.gateway(), ip("172.30.0.1"));
    assert_eq!(s.prefix(), 16);
    assert!(s.contains(ip("172.30.200.9")));
    assert!(!s.contains(ip("172.31.0.1")));
    assert_eq!(s.to_string(), "172.30.0.0/16");
}

#[test]
fn subnet_normalises_host_bits_and_rejects_bad_input() {
    assert_eq!(Subnet::parse("172.30.5.9/16").unwrap().to_string(), "172.30.0.0/16");
    for bad in ["172.30.0.0", "172.30.0.0/33", "172.30.0.0/31", "x/16", "172.30.0.0/abc"] {
        assert!(Subnet::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn allocation_starts_after_gateway_and_is_idempotent_per_owner() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    assert_eq!(ipam.allocate("a").unwrap(), ip("172.30.0.2"));
    assert_eq!(ipam.allocate("b").unwrap(), ip("172.30.0.3"));
    assert_eq!(ipam.allocate("a").unwrap(), ip("172.30.0.2"));
}

#[test]
fn release_frees_the_address_for_reuse() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    ipam.allocate("a").unwrap();
    ipam.allocate("b").unwrap();
    ipam.release("a");
    assert_eq!(ipam.allocate("c").unwrap(), ip("172.30.0.2"));
}

#[test]
fn reserve_conflicts_between_owners_but_not_for_the_same_owner() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    ipam.reserve(ip("172.30.0.50"), "a").unwrap();
    ipam.reserve(ip("172.30.0.50"), "a").unwrap();
    assert!(matches!(ipam.reserve(ip("172.30.0.50"), "b"), Err(Error::Conflict(_))));
    assert_eq!(ipam.owner_of(ip("172.30.0.50")), Some("a"));
}

#[test]
fn reserve_rejects_gateway_network_broadcast_and_outsiders() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    for bad in ["172.30.0.1", "172.30.0.0", "172.30.255.255", "10.0.0.5"] {
        assert!(ipam.reserve(ip(bad), "a").is_err(), "{bad}");
    }
}

#[test]
fn deterministic_addresses_are_stable_across_instances() {
    let mut a = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    let mut b = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    let x = a.for_service("myapp", "db", 0).unwrap();
    let y = b.for_service("myapp", "db", 0).unwrap();
    assert_eq!(x, y);
    assert!(a.subnet().contains(x));
    assert_ne!(x, a.subnet().gateway());
    assert_ne!(a.for_service("myapp", "db", 1).unwrap(), x);
}

#[test]
fn deterministic_addresses_avoid_the_vip_block() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    for i in 0..500 {
        let addr = ipam.for_service("p", "svc", i).unwrap();
        assert!(!ipam.is_vip(addr), "{addr}");
    }
}

#[test]
fn deterministic_allocation_is_idempotent_and_probes_on_collision() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    let first = ipam.for_service("p", "db", 0).unwrap();
    assert_eq!(ipam.for_service("p", "db", 0).unwrap(), first);
    let mut other = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    other.reserve(first, "squatter").unwrap();
    let moved = other.for_service("p", "db", 0).unwrap();
    assert_ne!(moved, first);
    assert_eq!(other.for_service("p", "db", 0).unwrap(), moved);
}

#[test]
fn vips_live_in_the_reserved_top_block() {
    let mut ipam = Ipam::new(Subnet::parse("172.30.0.0/16").unwrap());
    let v = ipam.vip("p", "web").unwrap();
    assert!(ipam.is_vip(v));
    assert!(v.octets()[2] == 255);
    assert_eq!(ipam.vip("p", "web").unwrap(), v);
    assert_ne!(ipam.vip("p", "api").unwrap(), v);
}

#[test]
fn rebuild_detects_duplicate_addresses() {
    let s = Subnet::parse("172.30.0.0/16").unwrap();
    let ok = Ipam::rebuild(s.clone(), [("a".to_string(), ip("172.30.0.5")), ("b".to_string(), ip("172.30.0.6"))]);
    assert!(ok.is_ok());
    let dup = Ipam::rebuild(s, [("a".to_string(), ip("172.30.0.5")), ("b".to_string(), ip("172.30.0.5"))]);
    assert!(matches!(dup, Err(Error::Conflict(_))));
}

#[test]
fn small_subnets_exhaust_cleanly() {
    let mut ipam = Ipam::new(Subnet::parse("10.9.0.0/29").unwrap());
    let mut got = 0;
    for i in 0..20 {
        match ipam.allocate(&format!("o{i}")) {
            Ok(_) => got += 1,
            Err(e) => {
                assert!(matches!(e, Error::Conflict(_)));
                break;
            }
        }
    }
    assert!((1..6).contains(&got));
}
