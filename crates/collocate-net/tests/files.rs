use collocate_net::files::{parse_nameservers, render_hosts, render_resolv};
use std::net::{IpAddr, Ipv4Addr};

#[test]
fn nameservers_skip_loopback_stub_and_comments() {
    let text = "# comment\nnameserver 127.0.0.53\nnameserver 10.0.0.2\nnameserver ::1\nnameserver 2001:4860:4860::8888\nsearch example.com\nnameserver bogus\n";
    let ns = parse_nameservers(text);
    assert_eq!(ns, vec!["10.0.0.2".parse::<IpAddr>().unwrap(), "2001:4860:4860::8888".parse().unwrap()]);
}

#[test]
fn nameservers_deduplicate_preserving_order() {
    let ns = parse_nameservers("nameserver 1.1.1.1\nnameserver 8.8.8.8\nnameserver 1.1.1.1\n");
    assert_eq!(ns.len(), 2);
    assert_eq!(ns[0].to_string(), "1.1.1.1");
}

#[test]
fn resolv_renders_nameservers_and_search() {
    let out = render_resolv(&["10.0.0.2".parse().unwrap()], &["corp.example".to_string()]);
    assert_eq!(out, "nameserver 10.0.0.2\nsearch corp.example\n");
    assert_eq!(render_resolv(&[], &[]), "");
}

#[test]
fn hosts_has_localhost_self_and_peers() {
    let out = render_hosts(
        "web1",
        Ipv4Addr::new(172, 30, 0, 3),
        &[("db".to_string(), Ipv4Addr::new(172, 30, 0, 2))],
        &[("legacy".to_string(), "10.1.1.1".parse().unwrap())],
    );
    assert!(out.contains("127.0.0.1\tlocalhost\n"));
    assert!(out.contains("::1\tlocalhost ip6-localhost ip6-loopback\n"));
    assert!(out.contains("172.30.0.3\tweb1\n"));
    assert!(out.contains("172.30.0.2\tdb\n"));
    assert!(out.contains("10.1.1.1\tlegacy\n"));
}
