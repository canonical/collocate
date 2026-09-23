use collocate_core::request::HealthState;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

pub fn tcp_check(addr: Ipv4Addr, port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((addr, port)), timeout).is_ok()
}

pub fn http_check(addr: Ipv4Addr, port: u16, path: &str, timeout: Duration) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&SocketAddr::from((addr, port)), timeout) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(timeout));
    let _ = s.set_write_timeout(Some(timeout));
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nUser-Agent: collocate-health\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let Ok(n) = s.read(&mut buf) else { return false };
    let head = String::from_utf8_lossy(&buf[..n]);
    head.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).is_some_and(|c| (200..400).contains(&c))
}

#[derive(Debug, Clone)]
pub struct HealthTracker {
    state: HealthState,
    consecutive_failures: u32,
    retries: u32,
}

impl HealthTracker {
    pub fn new(retries: u32) -> Self {
        HealthTracker { state: HealthState::Starting, consecutive_failures: 0, retries: retries.max(1) }
    }

    pub fn state(&self) -> HealthState {
        self.state
    }

    pub fn record(&mut self, ok: bool) -> Option<HealthState> {
        let before = self.state;
        if ok {
            self.consecutive_failures = 0;
            self.state = HealthState::Healthy;
        } else {
            self.consecutive_failures += 1;
            if self.consecutive_failures >= self.retries {
                self.state = HealthState::Unhealthy;
            }
        }
        (self.state != before).then_some(self.state)
    }
}
