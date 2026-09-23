use collocate_core::net::Proto;
use collocate_core::procinfo::{listening_ports, parse_stat, uptime_since, ProcState};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    pub state: ProcState,
}

pub fn main_process(procs: &[ProcEntry]) -> Option<&ProcEntry> {
    let pids: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
    let init = procs.iter().filter(|p| !pids.contains(&p.ppid)).min_by_key(|p| p.pid)?;
    procs.iter().filter(|p| p.ppid == init.pid).min_by_key(|p| p.pid).or(Some(init))
}

pub fn format_process(procs: &[ProcEntry]) -> String {
    let Some(main) = main_process(procs) else { return "—".to_string() };
    let extras = if procs.len() > 2 { procs.len() - 2 } else { 0 };
    let suffix = if extras > 0 { format!("+{extras}") } else { String::new() };
    format!("{} ({}, {}){suffix}", main.comm, main.state.word(), main.pid)
}

pub fn format_ports(tcp: &[u16], udp: &[u16]) -> String {
    let mut parts: Vec<String> = tcp.iter().map(|p| format!("{p}/tcp")).collect();
    parts.extend(udp.iter().map(|p| format!("{p}/udp")));
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join(",")
    }
}

pub fn format_uptime(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, mins, secs) = (s / 86400, s % 86400 / 3600, s % 3600 / 60, s % 60);
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else if mins > 0 {
        format!("{mins}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

pub fn read_procs(cgroup_dir: &Path) -> Vec<ProcEntry> {
    let Ok(text) = fs::read_to_string(cgroup_dir.join("cgroup.procs")) else { return Vec::new() };
    text.lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?).ok()?;
            Some(ProcEntry { pid, ppid: stat.ppid, comm: stat.comm, state: stat.state })
        })
        .collect()
}

pub fn read_listening(pid: u32) -> (Vec<u16>, Vec<u16>) {
    let read = |f: &str| fs::read_to_string(format!("/proc/{pid}/net/{f}")).unwrap_or_default();
    let mut tcp = listening_ports(&read("tcp"), Proto::Tcp);
    tcp.extend(listening_ports(&read("tcp6"), Proto::Tcp));
    let mut udp = listening_ports(&read("udp"), Proto::Udp);
    udp.extend(listening_ports(&read("udp6"), Proto::Udp));
    for v in [&mut tcp, &mut udp] {
        v.sort_unstable();
        v.dedup();
    }
    (tcp, udp)
}

pub fn read_uptime(pid: u32) -> Option<Duration> {
    let stat = parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?).ok()?;
    let system: f64 = fs::read_to_string("/proc/uptime").ok()?.split_whitespace().next()?.parse().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    Some(uptime_since(stat.starttime, hz, Duration::from_secs_f64(system)))
}
