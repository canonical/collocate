use crate::preseed::{LxdTarget, Mode, NodeRecord, Preseed};
use collocate_core::layout::DAEMON_PLUGS;
use collocate_core::settings::DaemonSettings;
use collocate_core::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

pub const PROFILE: &str = "collocate";
pub const REMOTE_SNAP: &str = "/root/collocate.snap";
pub const READY_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    Channel(String),
    SnapFile(String),
    Deb(Vec<String>),
}

impl Install {
    pub fn parse(s: &str) -> Result<Install> {
        let (kind, value) = s
            .split_once(':')
            .ok_or_else(|| Error::Invalid(format!("install {s:?}: expected channel:NAME, file:PATH or deb:PATH[,PATH...]")))?;
        if value.is_empty() {
            return Err(Error::Invalid(format!("install {s:?} has an empty value")));
        }
        match kind {
            "channel" => Ok(Install::Channel(value.to_string())),
            "file" => Ok(Install::SnapFile(value.to_string())),
            "deb" => Ok(Install::Deb(value.split(',').filter(|p| !p.is_empty()).map(String::from).collect())),
            other => Err(Error::Invalid(format!("unknown install mode {other:?}; expected channel, file or deb"))),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Install::Channel(c) => format!("channel:{c}"),
            Install::SnapFile(p) => format!("file:{p}"),
            Install::Deb(p) => format!("deb:{}", p.join(",")),
        }
    }

    pub fn relay(&self) -> &'static str {
        match self {
            Install::Deb(_) => "collocate-relay",
            _ => "collocate.relay",
        }
    }

    pub fn files(&self) -> Vec<String> {
        match self {
            Install::Channel(_) => Vec::new(),
            Install::SnapFile(p) => vec![p.clone()],
            Install::Deb(p) => p.clone(),
        }
    }

    pub fn is_snap(&self) -> bool {
        !matches!(self, Install::Deb(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Profile,
    Launch,
    Start,
    WaitReady,
    Push,
    Install,
    Connect,
    Enable,
    Init,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub node: String,
    pub kind: StepKind,
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

impl Step {
    pub fn describe(&self) -> String {
        let what = match self.kind {
            StepKind::Profile => "Creating the collocate LXD profile",
            StepKind::Launch => "Launching",
            StepKind::Start => "Starting",
            StepKind::WaitReady => "Waiting for",
            StepKind::Push => "Copying the package to",
            StepKind::Install => "Installing collocate on",
            StepKind::Connect => "Connecting interfaces on",
            StepKind::Enable => "Enabling the daemon on",
            StepKind::Init => "Initializing collocate on",
        };
        if self.node.is_empty() {
            what.to_string()
        } else {
            format!("{what} {}", self.node)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    pub profile: bool,
    pub instances: BTreeMap<String, String>,
    pub installed: BTreeSet<String>,
    pub initialized: BTreeSet<String>,
}

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

pub fn exec(target: &LxdTarget, node: &str, argv: Vec<String>) -> Vec<String> {
    let mut a = target.args(vec!["exec".into(), target.instance(node)]);
    a.push("--".into());
    a.extend(argv);
    a
}

fn sh(target: &LxdTarget, node: &str, script: &str) -> Vec<String> {
    exec(target, node, s(&["sh", "-c", script]))
}

fn memory_arg(input: &str) -> String {
    let t = input.trim();
    match t.chars().last().map(|c| c.to_ascii_lowercase()) {
        Some('k') => format!("{}KiB", &t[..t.len() - 1]),
        Some('m') => format!("{}MiB", &t[..t.len() - 1]),
        Some('g') => format!("{}GiB", &t[..t.len() - 1]),
        Some('t') => format!("{}TiB", &t[..t.len() - 1]),
        _ => format!("{t}B"),
    }
}

pub fn profile_steps(target: &LxdTarget) -> Vec<Step> {
    let name = format!("{}{PROFILE}", target.scope());
    vec![
        Step { node: String::new(), kind: StepKind::Profile, args: target.args(s(&["profile", "create", &name])), stdin: None },
        Step {
            node: String::new(),
            kind: StepKind::Profile,
            args: target.args(s(&["profile", "set", &name, "security.nesting=true"])),
            stdin: None,
        },
    ]
}

pub fn launch_args(target: &LxdTarget, node: &str, rec: &NodeRecord, default_image: &str) -> Vec<String> {
    let image = rec.image.clone().unwrap_or_else(|| default_image.to_string());
    let mut a = s(&["launch", &image, &target.instance(node), "--profile", "default", "--profile", PROFILE]);
    if let Some(t) = &rec.target {
        a.push("--target".into());
        a.push(t.clone());
    }
    if let Some(c) = rec.cpus {
        a.push("-c".into());
        a.push(format!("limits.cpu={}", c.ceil() as u64));
    }
    if let Some(m) = &rec.memory {
        a.push("-c".into());
        a.push(format!("limits.memory={}", memory_arg(m)));
    }
    target.args(a)
}

pub fn ready_script(snap: bool) -> String {
    let probe = if snap {
        "ip -4 route show default | grep -q . && getent hosts api.snapcraft.io >/dev/null"
    } else {
        "ip -4 route show default | grep -q ."
    };
    let mut script = format!("t=0; until {probe}; do t=$((t+1)); [ $t -gt {READY_TIMEOUT_SECS} ] && exit 1; sleep 1; done");
    if snap {
        script.push_str("; snap wait system seed.loaded");
    }
    script
}

pub fn node_preseed(settings: &DaemonSettings, node: &str) -> Result<String> {
    let mut daemon = settings.clone();
    daemon.node_name = Some(node.to_string());
    Preseed { mode: Mode::Local, daemon, cluster: None }.to_yaml()
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "package".into())
}

pub fn install_steps(target: &LxdTarget, node: &str, install: &Install) -> Vec<Step> {
    let step = |kind, args| Step { node: node.to_string(), kind, args, stdin: None };
    let mut out = Vec::new();
    match install {
        Install::Channel(ch) => {
            out.push(step(StepKind::Install, exec(target, node, s(&["snap", "install", "collocate", &format!("--channel={ch}")]))));
        }
        Install::SnapFile(path) => {
            out.push(step(StepKind::Push, target.args(s(&["file", "push", path, &format!("{}{REMOTE_SNAP}", target.instance(node))]))));
            out.push(step(StepKind::Install, exec(target, node, s(&["snap", "install", "--dangerous", REMOTE_SNAP]))));
        }
        Install::Deb(paths) => {
            let mut remote = Vec::new();
            for p in paths {
                let dest = format!("/root/{}", file_name(p));
                out.push(step(StepKind::Push, target.args(s(&["file", "push", p, &format!("{}{dest}", target.instance(node))]))));
                remote.push(dest);
            }
            let mut argv = s(&["env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y", "-q"]);
            argv.extend(remote);
            out.push(step(StepKind::Install, sh(target, node, "apt-get update -q >/dev/null")));
            out.push(step(StepKind::Install, exec(target, node, argv)));
            out.push(step(StepKind::Enable, exec(target, node, s(&["systemctl", "enable", "--now", "collocated"]))));
        }
    }
    if install.is_snap() {
        for plug in DAEMON_PLUGS {
            out.push(step(StepKind::Connect, exec(target, node, s(&["snap", "connect", &format!("collocate:{plug}")]))));
        }
    }
    out
}

pub fn plan(
    target: &LxdTarget,
    nodes: &BTreeMap<String, NodeRecord>,
    default_image: &str,
    install: &Install,
    settings: &DaemonSettings,
    observed: &Observed,
) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    if !observed.profile && nodes.keys().any(|n| !observed.instances.contains_key(n)) {
        steps.extend(profile_steps(target));
    }
    for (name, rec) in nodes {
        match observed.instances.get(name).map(String::as_str) {
            None => steps.push(Step {
                node: name.clone(),
                kind: StepKind::Launch,
                args: launch_args(target, name, rec, default_image),
                stdin: None,
            }),
            Some("Running") => {}
            Some(_) => steps.push(Step {
                node: name.clone(),
                kind: StepKind::Start,
                args: target.args(s(&["start", &target.instance(name)])),
                stdin: None,
            }),
        }
        let installed = observed.installed.contains(name);
        let initialized = observed.initialized.contains(name);
        if installed && initialized {
            continue;
        }
        steps.push(Step {
            node: name.clone(),
            kind: StepKind::WaitReady,
            args: sh(target, name, &ready_script(install.is_snap())),
            stdin: None,
        });
        if !installed {
            steps.extend(install_steps(target, name, install));
        } else if install.is_snap() {
            steps.extend(install_steps(target, name, install).into_iter().filter(|s| s.kind == StepKind::Connect));
        }
        steps.push(Step {
            node: name.clone(),
            kind: StepKind::Init,
            args: exec(target, name, s(&["collocate", "init", "--preseed"])),
            stdin: Some(node_preseed(settings, name)?),
        });
    }
    Ok(steps)
}

pub fn list_args(target: &LxdTarget) -> Vec<String> {
    target.args(s(&["list", &target.scope(), "--format", "json"]))
}

pub fn profile_show_args(target: &LxdTarget) -> Vec<String> {
    target.args(s(&["profile", "show", &format!("{}{PROFILE}", target.scope())]))
}

pub fn installed_args(target: &LxdTarget, node: &str) -> Vec<String> {
    sh(target, node, "command -v collocate >/dev/null || test -x /snap/bin/collocate")
}

pub fn info_args(target: &LxdTarget, node: &str) -> Vec<String> {
    exec(target, node, s(&["collocate", "info", "--format", "json"]))
}

pub fn delete_args(target: &LxdTarget, node: &str) -> Vec<String> {
    target.args(s(&["delete", "--force", &target.instance(node)]))
}

pub fn instance_states(json: &str) -> Result<BTreeMap<String, String>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = v.as_array().ok_or_else(|| Error::Parse("lxc list did not return an array".into()))?;
    Ok(arr.iter().filter_map(|i| Some((i["name"].as_str()?.to_string(), i["status"].as_str().unwrap_or("Unknown").to_string()))).collect())
}

pub fn reports_initialized(info_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(info_json).ok().and_then(|v| v["initialized"].as_bool()).unwrap_or(false)
}

pub fn cluster_members(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|m| m["server_name"].as_str().map(String::from)).collect()))
        .unwrap_or_default()
}
