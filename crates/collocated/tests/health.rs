use collocate_core::request::HealthState;
use collocated::health::{http_check, tcp_check, HealthTracker};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

const T: Duration = Duration::from_millis(500);

#[test]
fn tcp_check_reflects_listening_state() {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    assert!(tcp_check(Ipv4Addr::LOCALHOST, port, T));
    drop(l);
    assert!(!tcp_check(Ipv4Addr::LOCALHOST, port, T));
}

fn serve(status: &'static str) -> u16 {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().take(4) {
            let mut s = s.unwrap();
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf);
            let _ = s.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes());
        }
    });
    port
}

#[test]
fn http_check_accepts_success_and_redirects_only() {
    assert!(http_check(Ipv4Addr::LOCALHOST, serve("200 OK"), "/healthz", T));
    assert!(http_check(Ipv4Addr::LOCALHOST, serve("302 Found"), "/", T));
    assert!(!http_check(Ipv4Addr::LOCALHOST, serve("500 Internal Server Error"), "/", T));
    assert!(!http_check(Ipv4Addr::LOCALHOST, serve("404 Not Found"), "/", T));
}

#[test]
fn http_check_fails_when_nothing_answers() {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    assert!(!http_check(Ipv4Addr::LOCALHOST, port, "/", T));
}

#[test]
fn tracker_starts_starting_becomes_healthy_on_first_success() {
    let mut t = HealthTracker::new(3);
    assert_eq!(t.state(), HealthState::Starting);
    assert_eq!(t.record(true), Some(HealthState::Healthy));
    assert_eq!(t.record(true), None);
}

#[test]
fn tracker_needs_consecutive_failures_to_turn_unhealthy() {
    let mut t = HealthTracker::new(3);
    t.record(true);
    assert_eq!(t.record(false), None);
    assert_eq!(t.record(false), None);
    assert_eq!(t.record(true), None);
    assert_eq!(t.record(false), None);
    assert_eq!(t.record(false), None);
    assert_eq!(t.record(false), Some(HealthState::Unhealthy));
    assert_eq!(t.record(false), None);
    assert_eq!(t.record(true), Some(HealthState::Healthy));
}

#[test]
fn a_replica_that_never_comes_up_becomes_unhealthy_after_the_retries() {
    let mut t = HealthTracker::new(2);
    assert_eq!(t.record(false), None);
    assert_eq!(t.record(false), Some(HealthState::Unhealthy));
}

#[test]
fn retries_of_zero_behave_like_one() {
    let mut t = HealthTracker::new(0);
    t.record(true);
    assert_eq!(t.record(false), Some(HealthState::Unhealthy));
}
