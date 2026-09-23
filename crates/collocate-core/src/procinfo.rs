use crate::net::Proto;
use crate::{Error, Result};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Sleeping,
    DiskWait,
    Zombie,
    Stopped,
    Other(char),
}

impl ProcState {
    pub fn word(&self) -> &'static str {
        match self {
            ProcState::Running => "R",
            ProcState::Sleeping => "S",
            ProcState::DiskWait => "D",
            ProcState::Zombie => "Z",
            ProcState::Stopped => "T",
            ProcState::Other(_) => "?",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub pid: u32,
    pub comm: String,
    pub state: ProcState,
    pub ppid: u32,
    pub starttime: u64,
}

pub fn parse_stat(line: &str) -> Result<Stat> {
    let bad = || Error::Parse(format!("stat line: {line}"));
    let open = line.find('(').ok_or_else(bad)?;
    let close = line.rfind(')').ok_or_else(bad)?;
    if close < open {
        return Err(bad());
    }
    let pid = line[..open].trim().parse().map_err(|_| bad())?;
    let comm = line[open + 1..close].to_string();
    let fields: Vec<&str> = line[close + 1..].split_whitespace().collect();
    if fields.len() < 20 {
        return Err(bad());
    }
    let state = match fields[0].chars().next().ok_or_else(bad)? {
        'R' => ProcState::Running,
        'S' => ProcState::Sleeping,
        'D' => ProcState::DiskWait,
        'Z' => ProcState::Zombie,
        'T' | 't' => ProcState::Stopped,
        c => ProcState::Other(c),
    };
    Ok(Stat { pid, comm, state, ppid: fields[1].parse().map_err(|_| bad())?, starttime: fields[19].parse().map_err(|_| bad())? })
}

fn port_of(addr: &str) -> Option<u16> {
    let hex = addr.rsplit(':').next()?;
    u16::from_str_radix(hex, 16).ok()
}

pub fn listening_ports(text: &str, proto: Proto) -> Vec<u16> {
    let mut ports: Vec<u16> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 4 {
                return None;
            }
            let local = port_of(cols[1])?;
            let listening = match proto {
                Proto::Tcp => cols[3] == "0A",
                Proto::Udp => port_of(cols[2]) == Some(0),
            };
            listening.then_some(local)
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupEvents {
    pub populated: bool,
}

fn kv_lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines().filter_map(|l| l.split_once(' '))
}

pub fn parse_cgroup_events(text: &str) -> Result<CgroupEvents> {
    kv_lines(text)
        .find(|(k, _)| *k == "populated")
        .map(|(_, v)| CgroupEvents { populated: v.trim() == "1" })
        .ok_or_else(|| Error::Parse("cgroup.events lacks populated".into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuStat {
    pub usage_usec: u64,
    pub nr_throttled: u64,
    pub throttled_usec: u64,
}

pub fn parse_cpu_stat(text: &str) -> Result<CpuStat> {
    let mut out = CpuStat::default();
    let mut seen = false;
    for (k, v) in kv_lines(text) {
        let n: u64 = match v.trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        match k {
            "usage_usec" => {
                out.usage_usec = n;
                seen = true;
            }
            "nr_throttled" => out.nr_throttled = n,
            "throttled_usec" => out.throttled_usec = n,
            _ => {}
        }
    }
    if seen {
        Ok(out)
    } else {
        Err(Error::Parse("cpu.stat lacks usage_usec".into()))
    }
}

pub fn uptime_since(start_ticks: u64, hz: u64, system_uptime: Duration) -> Duration {
    let started = Duration::from_secs_f64(start_ticks as f64 / hz.max(1) as f64);
    system_uptime.saturating_sub(started)
}
