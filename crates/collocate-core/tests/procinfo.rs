use collocate_core::net::Proto;
use collocate_core::procinfo::{listening_ports, parse_cgroup_events, parse_cpu_stat, parse_stat, uptime_since, ProcState};
use std::time::Duration;

#[test]
fn stat_parses_simple() {
    let s = parse_stat("4211 (postgres) S 4200 4211 4211 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 987654 1000 2000").unwrap();
    assert_eq!(s.pid, 4211);
    assert_eq!(s.comm, "postgres");
    assert_eq!(s.state, ProcState::Sleeping);
    assert_eq!(s.ppid, 4200);
    assert_eq!(s.starttime, 987654);
}

#[test]
fn stat_handles_awkward_comm() {
    let s = parse_stat("77 (we) (ird) R 1 77 77 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0").unwrap();
    assert_eq!(s.comm, "we) (ird");
    assert_eq!(s.state, ProcState::Running);
}

#[test]
fn stat_states_map() {
    for (c, st) in [
        ("R", ProcState::Running),
        ("S", ProcState::Sleeping),
        ("D", ProcState::DiskWait),
        ("Z", ProcState::Zombie),
        ("T", ProcState::Stopped),
    ] {
        let line = format!("1 (x) {c} 0 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0");
        assert_eq!(parse_stat(&line).unwrap().state, st);
    }
}

#[test]
fn stat_rejects_truncated() {
    assert!(parse_stat("1 (x) S 0").is_err());
    assert!(parse_stat("garbage").is_err());
}

const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0
   1: 0100007F:0035 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12346 1 0000000000000000 100 0 0 10 0
   2: 0100007F:9C40 0100007F:1F90 01 00000000:00000000 00:00000000 00000000     0        0 12347 1 0000000000000000 100 0 0 10 0
   3: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12348 1 0000000000000000 100 0 0 10 0
";

#[test]
fn tcp_listening_ports_only_listen_state_sorted_unique() {
    assert_eq!(listening_ports(TCP, Proto::Tcp), vec![53, 8080]);
}

#[test]
fn tcp6_addresses_are_supported() {
    let t = "  sl  local_address                         remote_address                        st
   0: 00000000000000000000000000000000:1538 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 1 1 0 100 0 0 10 0
";
    assert_eq!(listening_ports(t, Proto::Tcp), vec![5432]);
}

#[test]
fn udp_counts_unconnected_bound_sockets() {
    let t = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
   0: 00000000:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 1 2 0000000000000000 0
   1: 0100007F:1234 0100007F:0050 01 00000000:00000000 00:00000000 00000000     0        0 1 2 0000000000000000 0
";
    assert_eq!(listening_ports(t, Proto::Udp), vec![53]);
}

#[test]
fn cgroup_events_populated() {
    assert!(parse_cgroup_events("populated 1\nfrozen 0\n").unwrap().populated);
    assert!(!parse_cgroup_events("populated 0\nfrozen 0\n").unwrap().populated);
    assert!(parse_cgroup_events("frozen 0\n").is_err());
}

#[test]
fn cpu_stat_fields() {
    let s = parse_cpu_stat("usage_usec 12345\nuser_usec 1\nsystem_usec 2\nnr_periods 10\nnr_throttled 3\nthrottled_usec 999\n").unwrap();
    assert_eq!(s.usage_usec, 12345);
    assert_eq!(s.nr_throttled, 3);
    assert_eq!(s.throttled_usec, 999);
}

#[test]
fn cpu_stat_without_throttle_fields_defaults_zero() {
    let s = parse_cpu_stat("usage_usec 7\n").unwrap();
    assert_eq!((s.nr_throttled, s.throttled_usec), (0, 0));
}

#[test]
fn uptime_from_start_ticks() {
    let d = uptime_since(98765, 100, Duration::from_secs(2000));
    assert_eq!(d.as_secs(), 1012);
}

#[test]
fn uptime_never_negative() {
    assert_eq!(uptime_since(500000, 100, Duration::from_secs(10)), Duration::ZERO);
}
