use crate::setup::{check_settings, missing_interfaces, snap_interfaces, write_config};
use collocate_core::layout::Layout;
use collocate_core::request::{Request, Response};
use collocate_core::settings::{DaemonSettings, DEFAULT_GROUP};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::{Error, Result};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

pub const NOT_INITIALIZED: &str = "collocate is not initialized; run 'sudo collocate init'";

pub fn group_gid(name: &str) -> Option<u32> {
    fs::read_to_string("/etc/group").ok()?.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.first() == Some(&name)).then(|| f.get(2)?.parse().ok()).flatten()
    })
}

pub fn bind_socket(path: &Path, group: &str) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
    if let Some(gid) = group_gid(group) {
        let _ = collocate_sys::misc::chown(path.to_str().unwrap_or(""), 0, gid);
    }
    Ok(listener)
}

pub fn interfaces_json() -> serde_json::Value {
    serde_json::Value::Array(snap_interfaces().into_iter().map(|(p, ok)| serde_json::json!({"plug": p, "connected": ok})).collect())
}

pub fn checks_json() -> serde_json::Value {
    let probe = collocate_sys::probe::probe();
    serde_json::Value::Array(probe.checks.iter().map(|c| serde_json::json!({"name": c.name, "ok": c.ok, "detail": c.detail})).collect())
}

fn info() -> String {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "initialized": false,
        "kernel": fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim(),
        "checks": checks_json(),
        "interfaces": interfaces_json(),
    })
    .to_string()
}

pub fn require_root(stream: &UnixStream) -> Result<()> {
    match collocate_sys::misc::peer_uid(stream.as_raw_fd()) {
        Ok(0) => Ok(()),
        Ok(uid) => Err(Error::Denied(format!("only root can initialize collocate (caller uid {uid})"))),
        Err(e) => Err(Error::Denied(format!("cannot identify caller: {e}"))),
    }
}

pub fn require_interfaces() -> Result<()> {
    let missing = missing_interfaces();
    if missing.is_empty() {
        return Ok(());
    }
    let cmds = collocate_core::layout::connect_commands("collocate", &missing);
    Err(Error::Invalid(format!("the snap is missing required interfaces; run:\n  {}", cmds.join("\n  "))))
}

fn init(stream: &UnixStream, config_path: &Path, settings: &DaemonSettings) -> Result<()> {
    require_root(stream)?;
    require_interfaces()?;
    check_settings(settings, None)?;
    write_config(config_path, settings)
}

enum Outcome {
    Continue,
    Initialized,
    Shutdown,
}

fn serve_conn(mut stream: UnixStream, config_path: &Path) -> Outcome {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    loop {
        let req: Request = match read_frame(&mut stream) {
            Ok(r) => r,
            Err(_) => return Outcome::Continue,
        };
        let (resp, outcome) = match req {
            Request::Info => (Response::Text { text: info() }, None),
            Request::Init { settings, .. } => match init(&stream, config_path, &settings) {
                Ok(()) => (Response::Ok, Some(Outcome::Initialized)),
                Err(e) => (Response::error(&e), None),
            },
            Request::Shutdown => match require_root(&stream) {
                Ok(()) => (Response::Ok, Some(Outcome::Shutdown)),
                Err(e) => (Response::error(&e), None),
            },
            _ => (Response::error(&Error::NotInitialized(NOT_INITIALIZED.into())), None),
        };
        let _ = write_frame(&mut stream, &resp);
        if let Some(o) = outcome {
            return o;
        }
    }
}

pub fn serve(layout: &Layout, config_path: &Path) -> Result<bool> {
    let socket = layout.socket();
    let listener = bind_socket(&socket, DEFAULT_GROUP)?;
    eprintln!("collocated: not initialized, waiting for 'collocate init' on {}", socket.display());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        match serve_conn(stream, config_path) {
            Outcome::Continue => {}
            Outcome::Initialized => {
                let _ = fs::remove_file(&socket);
                return Ok(true);
            }
            Outcome::Shutdown => {
                let _ = fs::remove_file(&socket);
                return Ok(false);
            }
        }
    }
    Ok(false)
}
