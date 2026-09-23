use std::net::{IpAddr, Ipv4Addr};

pub fn parse_nameservers(text: &str) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("nameserver") {
            continue;
        }
        let Some(Ok(addr)) = parts.next().map(str::parse::<IpAddr>) else { continue };
        if addr.is_loopback() || addr.is_unspecified() || out.contains(&addr) {
            continue;
        }
        out.push(addr);
    }
    out
}

pub fn render_resolv(nameservers: &[IpAddr], search: &[String]) -> String {
    let mut out = String::new();
    for ns in nameservers {
        out.push_str(&format!("nameserver {ns}\n"));
    }
    if !search.is_empty() {
        out.push_str(&format!("search {}\n", search.join(" ")));
    }
    out
}

pub fn render_hosts(hostname: &str, addr: Ipv4Addr, peers: &[(String, Ipv4Addr)], extra: &[(String, IpAddr)]) -> String {
    let mut out = String::from("127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n");
    out.push_str(&format!("{addr}\t{hostname}\n"));
    for (name, a) in peers {
        out.push_str(&format!("{a}\t{name}\n"));
    }
    for (name, a) in extra {
        out.push_str(&format!("{a}\t{name}\n"));
    }
    out
}
