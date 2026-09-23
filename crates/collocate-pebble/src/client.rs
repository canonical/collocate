use crate::error::{Error, Result};
use crate::http;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckInfo {
    pub name: String,
    #[serde(default)]
    pub level: String,
    pub status: String,
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub threshold: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    #[serde(default)]
    pub startup: String,
    pub current: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub time: String,
    pub service: String,
    pub message: String,
}

impl LogEntry {
    pub fn nanos(&self) -> u64 {
        crate::time::rfc3339_nanos(&self.time).unwrap_or(0)
    }

    pub fn render(&self) -> String {
        let mut line = format!("{} [{}] {}", self.time, self.service, self.message);
        if !line.ends_with('\n') {
            line.push('\n');
        }
        line
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub healthy: bool,
    pub problems: Vec<String>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "status-code", default)]
    status_code: u16,
    #[serde(default)]
    result: serde_json::Value,
}

pub struct Client {
    socket: PathBuf,
    timeout: Duration,
}

fn query_escape(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b',') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

impl Client {
    pub fn new(socket: impl Into<PathBuf>, timeout: Duration) -> Client {
        Client { socket: socket.into(), timeout }
    }

    fn sync<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = http::get(&self.socket, path, self.timeout)?;
        let env: Envelope = serde_json::from_slice(&resp.body)?;
        let status = if env.status_code == 0 { resp.status } else { env.status_code };
        if !(200..300).contains(&status) {
            let message = env.result.get("message").and_then(|m| m.as_str()).unwrap_or("request failed").to_string();
            return Err(Error::Api { status, message });
        }
        Ok(serde_json::from_value(env.result)?)
    }

    pub fn checks(&self, level: Option<&str>) -> Result<Vec<CheckInfo>> {
        let path = match level {
            Some(l) => format!("/v1/checks?level={}", query_escape(l)),
            None => "/v1/checks".to_string(),
        };
        Ok(self.sync::<Option<Vec<CheckInfo>>>(&path)?.unwrap_or_default())
    }

    pub fn services(&self) -> Result<Vec<ServiceInfo>> {
        Ok(self.sync::<Option<Vec<ServiceInfo>>>("/v1/services")?.unwrap_or_default())
    }

    pub fn health(&self, level: Option<&str>) -> Result<Health> {
        let mut problems = Vec::new();
        for c in self.checks(level)? {
            if c.status == "down" {
                problems.push(format!("check {} is down ({}/{} failures)", c.name, c.failures, c.threshold));
            }
        }
        for s in self.services()? {
            if matches!(s.current.as_str(), "backoff" | "error") {
                problems.push(format!("service {} is in {}", s.name, s.current));
            }
        }
        Ok(Health { healthy: problems.is_empty(), problems })
    }

    pub fn logs(&self, services: &[String], n: Option<usize>) -> Result<Vec<LogEntry>> {
        let mut path = format!("/v1/logs?n={}", n.map_or(-1, |n| n as i64));
        if !services.is_empty() {
            path.push_str(&format!("&services={}", query_escape(&services.join(","))));
        }
        let resp = http::get(&self.socket, &path, self.timeout)?;
        if !(200..300).contains(&resp.status) || resp.content_type.starts_with("application/json") {
            let env: Envelope = serde_json::from_slice(&resp.body)?;
            let message = env.result.get("message").and_then(|m| m.as_str()).unwrap_or("request failed").to_string();
            return Err(Error::Api { status: resp.status, message });
        }
        let mut out = Vec::new();
        for line in resp.body.split(|b| *b == b'\n').filter(|l| !l.iter().all(u8::is_ascii_whitespace)) {
            out.push(serde_json::from_slice(line)?);
        }
        Ok(out)
    }
}
