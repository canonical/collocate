use crate::config::Config;
use collocate_core::layout::DAEMON_PLUGS;
use collocate_core::settings::{DaemonSettings, RootModeSetting};
use collocate_core::{Error, Result};
use collocate_net::ipam::Subnet;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const KEEP_INIT_COPIES: usize = 3;

pub fn stage_init(cfg: &Config) -> Result<PathBuf> {
    let bytes = fs::read(&cfg.init_path).map_err(|e| Error::Internal(format!("{}: {e}", cfg.init_path.display())))?;
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let dir = cfg.state_dir.join("bin");
    fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("collocate-init-{hex}"));
    if !dest.is_file() {
        let tmp = dir.join(format!(".collocate-init-{hex}.{}", std::process::id()));
        fs::write(&tmp, &bytes)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        fs::rename(&tmp, &dest)?;
    }
    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("collocate-init-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p != &dest)
        .collect();
    copies.sort();
    copies.reverse();
    for (_, old) in copies.into_iter().skip(KEEP_INIT_COPIES - 1) {
        let _ = fs::remove_file(old);
    }
    Ok(dest)
}

const SETTINGS_KEYS: [&str; 7] = ["subnet", "bridge", "root_mode", "group", "node_name", "defaults", "https_address"];

pub fn render_config(existing: Option<&str>, settings: &DaemonSettings) -> Result<String> {
    let mut table: toml::Table = match existing {
        Some(text) => toml::from_str(text).map_err(|e| Error::Invalid(format!("existing config: {e}")))?,
        None => toml::Table::new(),
    };
    for k in SETTINGS_KEYS {
        table.remove(k);
    }
    let fresh: toml::Table = toml::from_str(&settings.to_toml()?).map_err(|e| Error::Internal(e.to_string()))?;
    table.extend(fresh);
    toml::to_string(&table).map_err(|e| Error::Internal(format!("render config: {e}")))
}

pub fn write_config(path: &Path, settings: &DaemonSettings) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let text = render_config(existing.as_deref(), settings)?;
    Config::from_toml(&text)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.new");
    fs::write(&tmp, text)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn mask(addr: std::net::Ipv4Addr, prefix: u8) -> u32 {
    let bits = u32::from(addr);
    if prefix == 0 {
        0
    } else {
        bits & (u32::MAX << (32 - u32::from(prefix)))
    }
}

fn parse_cidr(s: &str) -> Option<(std::net::Ipv4Addr, u8)> {
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, p.parse::<u8>().ok().filter(|p| *p <= 32)?),
        None => (s, 32),
    };
    Some((addr.parse().ok()?, prefix))
}

pub fn overlaps(a: &Subnet, b: &str) -> bool {
    let Some((addr, prefix)) = parse_cidr(b) else { return false };
    let p = a.prefix().min(prefix);
    mask(a.network(), p) == mask(addr, p)
}

pub fn route_conflicts(routes_json: &str, subnet: &Subnet, own_bridges: &[&str]) -> Vec<String> {
    let routes: Vec<serde_json::Value> = serde_json::from_str(routes_json).unwrap_or_default();
    routes
        .iter()
        .filter_map(|r| {
            let dst = r["dst"].as_str()?;
            let dev = r["dev"].as_str().unwrap_or("");
            if dst == "default" || own_bridges.contains(&dev) {
                return None;
            }
            overlaps(subnet, dst).then(|| format!("{dst} on {dev}"))
        })
        .collect()
}

fn link_exists(name: &str) -> bool {
    Path::new("/sys/class/net").join(name).exists()
}

pub fn check_settings(settings: &DaemonSettings, current: Option<&Config>) -> Result<()> {
    settings.validate()?;
    let subnet = Subnet::parse(&settings.subnet)?;
    let current_bridge = current.map(|c| c.bridge.as_str());
    if current_bridge != Some(settings.bridge.as_str()) && link_exists(&settings.bridge) {
        return Err(Error::Conflict(format!("network interface {} already exists; choose another bridge name", settings.bridge)));
    }
    if let Ok(out) = Command::new("ip").args(["-j", "-4", "route", "show"]).output() {
        let mut own = vec![settings.bridge.as_str()];
        own.extend(current_bridge);
        let conflicts = route_conflicts(&String::from_utf8_lossy(&out.stdout), &subnet, &own);
        if !conflicts.is_empty() {
            return Err(Error::Conflict(format!("subnet {} overlaps existing routes: {}", settings.subnet, conflicts.join(", "))));
        }
    }
    let probe = collocate_sys::probe::probe();
    let ok = |name: &str| probe.check(name).is_some_and(|c| c.ok);
    match settings.root_mode {
        RootModeSetting::Overlay if !ok("overlayfs") => {
            Err(Error::Invalid("root mode overlay requested but overlayfs is unavailable".into()))
        }
        RootModeSetting::FuseOverlay if !ok("fuse-overlayfs") => {
            Err(Error::Invalid("root mode fuse-overlay requested but fuse-overlayfs is unavailable".into()))
        }
        _ => Ok(()),
    }
}

pub fn snap_interfaces() -> Vec<(String, bool)> {
    if !under_snapd() {
        return Vec::new();
    }
    DAEMON_PLUGS
        .iter()
        .map(|p| {
            let connected = Command::new("snapctl").args(["is-connected", p]).status().is_ok_and(|s| s.success());
            (p.to_string(), connected)
        })
        .collect()
}

pub fn missing_interfaces() -> Vec<String> {
    snap_interfaces().into_iter().filter(|(_, ok)| !ok).map(|(p, _)| p).collect()
}

fn run_quiet(prog: &str, args: &[&str]) {
    let _ = Command::new(prog).args(args).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
}

pub fn teardown_network(bridge: &str) {
    run_quiet("nft", &["delete", "table", "inet", "collocate"]);
    if link_exists(bridge) {
        run_quiet("ip", &["link", "del", bridge]);
    }
}

fn remove_cgroup_tree(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_cgroup_tree(&e.path());
            }
        }
    }
    let _ = fs::remove_dir(dir);
}

fn cgroup_empty(dir: &Path) -> bool {
    fs::read_to_string(dir.join("cgroup.events")).map(|t| t.lines().any(|l| l == "populated 0")).unwrap_or(true)
}

pub fn teardown(cfg: &Config) -> Result<()> {
    let slice = cfg.cgroup_root.join(&cfg.cgroup_slice);
    if slice.is_dir() {
        let _ = fs::write(slice.join("cgroup.kill"), "1");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cgroup_empty(&slice) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        remove_cgroup_tree(&slice);
    }
    teardown_network(&cfg.bridge);
    let _ = fs::remove_file(cfg.socket());
    if slice.exists() {
        return Err(Error::Internal(format!("{} could not be removed", slice.display())));
    }
    Ok(())
}

pub const RESTART_EXIT_CODE: i32 = 75;

pub fn under_snapd() -> bool {
    std::env::var_os("SNAP").is_some() && (std::env::var_os("SNAP_COOKIE").is_some() || std::env::var_os("SNAP_CONTEXT").is_some())
}

pub fn restart() -> ! {
    if under_snapd() {
        eprintln!("collocated: exiting so snapd restarts the service with its current interfaces");
        std::process::exit(RESTART_EXIT_CODE);
    }
    let e = reexec();
    eprintln!("collocated: {e}");
    std::process::exit(1);
}

pub fn reexec() -> Error {
    use std::os::unix::process::CommandExt;
    let err = Command::new("/proc/self/exe").args(std::env::args_os().skip(1)).exec();
    Error::Internal(format!("re-exec failed: {err}"))
}
