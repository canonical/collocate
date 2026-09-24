use collocate_core::ContainerId;
use collocate_net::ipam::Subnet;
use collocate_net::netcfg::{bridge_setup, in_ns_config, move_to_ns, teardown_veth, veth_setup, Cmd};

fn id() -> ContainerId {
    ContainerId::parse("7f3a9c02e1b4").unwrap()
}

fn c(parts: &[&str]) -> Cmd {
    Cmd(parts.iter().map(|s| s.to_string()).collect())
}

#[test]
fn bridge_setup_is_idempotent_friendly() {
    let cmds = bridge_setup("collocate0", &Subnet::parse("172.30.0.0/16").unwrap());
    assert!(cmds.contains(&c(&["ip", "link", "add", "name", "collocate0", "type", "bridge"])));
    assert!(cmds.contains(&c(&["ip", "addr", "replace", "172.30.0.1/16", "dev", "collocate0"])));
    assert!(cmds.contains(&c(&["ip", "link", "set", "collocate0", "up"])));
    assert!(cmds.contains(&c(&["sysctl", "-qw", "net.ipv4.ip_forward=1"])));
    assert!(cmds.contains(&c(&["sysctl", "-qw", "net.ipv4.conf.collocate0.route_localnet=1"])));
}

#[test]
fn veth_creation_attaches_the_host_end() {
    let cmds = veth_setup(&id(), "collocate0");
    assert_eq!(cmds[0], c(&["ip", "link", "add", "vh7f3a9c02e1", "type", "veth", "peer", "name", "vc7f3a9c02e1"]));
    assert!(cmds.contains(&c(&["ip", "link", "set", "vh7f3a9c02e1", "master", "collocate0"])));
    assert!(cmds.contains(&c(&["ip", "link", "set", "vh7f3a9c02e1", "up"])));
}

#[test]
fn peer_moves_into_the_child_namespace() {
    assert_eq!(move_to_ns(&id(), 4242), c(&["ip", "link", "set", "vc7f3a9c02e1", "netns", "4242"]));
}

#[test]
fn in_namespace_configuration_uses_nsenter() {
    let cmds = in_ns_config(4242, &id(), "172.30.0.5".parse().unwrap(), 16, "172.30.0.1".parse().unwrap(), true);
    let prefix = ["nsenter", "-t", "4242", "-n", "--"];
    assert!(cmds.iter().all(|cmd| cmd.0.iter().take(5).map(String::as_str).eq(prefix)));
    let tails: Vec<Vec<&str>> = cmds.iter().map(|cmd| cmd.0[5..].iter().map(String::as_str).collect()).collect();
    assert!(tails.contains(&vec!["ip", "link", "set", "lo", "up"]));
    assert!(tails.contains(&vec!["ip", "link", "set", "vc7f3a9c02e1", "name", "eth0"]));
    assert!(tails.contains(&vec!["ip", "addr", "add", "172.30.0.5/16", "dev", "eth0"]));
    assert!(tails.contains(&vec!["ip", "link", "set", "eth0", "up"]));
    assert!(tails.contains(&vec!["ip", "route", "add", "default", "via", "172.30.0.1"]));
    let (low, high) = collocate_net::netcfg::ping_group_range(&std::fs::read_to_string("/proc/self/gid_map").unwrap());
    let expected = format!("net.ipv4.ping_group_range={low} {high}");
    assert!(tails.contains(&vec!["sysctl", "-qw", expected.as_str()]), "{tails:?}");
}

#[test]
fn the_ping_group_range_fits_the_user_namespace() {
    use collocate_net::netcfg::ping_group_range;
    assert_eq!(ping_group_range("         0          0 4294967295\n"), (0, 2147483647));
    assert_eq!(ping_group_range("         0    1000000 1000000000\n"), (0, 999999999));
    assert_eq!(ping_group_range("         0    1000000      65536\n"), (0, 65535));
    assert_eq!(ping_group_range("      1000       1000          1\n"), (1, 0));
    assert_eq!(ping_group_range(""), (0, 2147483647));
}

#[test]
fn ping_group_is_optional_and_rename_precedes_addressing() {
    let cmds = in_ns_config(1, &id(), "172.30.0.5".parse().unwrap(), 16, "172.30.0.1".parse().unwrap(), false);
    assert!(!cmds.iter().any(|c| c.0.iter().any(|a| a.contains("ping_group_range"))));
    let pos = |needle: &str| cmds.iter().position(|c| c.0.iter().any(|a| a == needle)).unwrap();
    assert!(pos("name") < pos("172.30.0.5/16"));
    assert!(pos("172.30.0.5/16") < pos("via") + 1);
}

#[test]
fn deleting_the_host_end_removes_the_pair() {
    assert_eq!(teardown_veth(&id()), c(&["ip", "link", "del", "vh7f3a9c02e1"]));
}
