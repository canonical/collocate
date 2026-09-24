use crate::cli::{Cli, InitArgs, InitMode};
use crate::commands::{json, narrate, narrate_detail, read_stdin, socket_hint};
use crate::output::{success_line, Format};
use collocate_cluster::lxc::Lxc;
use collocate_cluster::preseed::{ClusterPreseed, LxdTarget, Mode, NodeRecord, Preseed, Registry};
use collocate_cluster::provision::{self, cluster_members, Install, Observed, StepKind};
use collocate_cluster::transport::LxcApi;
use collocate_core::client::Api;
use collocate_core::layout::{connect_commands, Layout};
use collocate_core::request::{Request, Response};
use collocate_core::settings::{DaemonSettings, RootModeSetting};
use collocate_core::{Error, Result};
use std::collections::{BTreeMap, VecDeque};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

const FATAL_CHECKS: [&str; 4] = ["cgroup2", "cgroup-controllers", "clone3", "pidfd"];
const NODE_PREFIX: &str = "collocate";

pub trait Prompter {
    fn ask(&mut self, question: &str, default: &str) -> Result<String>;
}

pub struct TerminalPrompter;

impl Prompter for TerminalPrompter {
    fn ask(&mut self, question: &str, default: &str) -> Result<String> {
        eprint!("{question} ({default}): ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            return Err(Error::Invalid("input ended before init finished".into()));
        }
        let answer = line.trim();
        Ok(if answer.is_empty() { default.to_string() } else { answer.to_string() })
    }
}

#[derive(Default)]
pub struct ScriptedPrompter {
    pub answers: VecDeque<String>,
    pub asked: Vec<String>,
}

impl ScriptedPrompter {
    pub fn new(answers: &[&str]) -> ScriptedPrompter {
        ScriptedPrompter { answers: answers.iter().map(|a| a.to_string()).collect(), asked: Vec::new() }
    }
}

impl Prompter for ScriptedPrompter {
    fn ask(&mut self, question: &str, default: &str) -> Result<String> {
        self.asked.push(question.to_string());
        let answer = self.answers.pop_front().unwrap_or_default();
        Ok(if answer.is_empty() { default.to_string() } else { answer })
    }
}

fn non_empty(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty() && t != "none").then(|| t.to_string())
}

fn ask_parsed<T>(p: &mut dyn Prompter, question: &str, default: &str, parse: impl Fn(&str) -> Result<T>) -> Result<T> {
    let mut last = None;
    for _ in 0..3 {
        let answer = p.ask(question, default)?;
        match parse(&answer) {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("{e}");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| Error::Invalid(format!("no valid answer to {question:?}"))))
}

fn parse_mode(s: &str) -> Result<Mode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "local" => Ok(Mode::Local),
        "lxd" => Ok(Mode::Lxd),
        other => Err(Error::Invalid(format!("answer local or lxd, not {other:?}"))),
    }
}

fn parse_count(s: &str) -> Result<u32> {
    match s.trim().parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(Error::Invalid(format!("expected a number of nodes of at least 1, got {s:?}"))),
    }
}

fn parse_optional_cpus(s: &str) -> Result<Option<f64>> {
    match non_empty(s.to_string()) {
        None => Ok(None),
        Some(v) => match v.parse::<f64>() {
            Ok(c) if c > 0.0 && c.is_finite() => Ok(Some(c)),
            _ => Err(Error::Invalid(format!("expected a positive number of CPUs, got {v:?}"))),
        },
    }
}

fn parse_optional_size(s: &str) -> Result<Option<String>> {
    match non_empty(s.to_string()) {
        None => Ok(None),
        Some(v) => {
            collocate_core::size::parse_size(&v)?;
            Ok(Some(v))
        }
    }
}

fn mode_of(m: InitMode) -> Mode {
    match m {
        InitMode::Local => Mode::Local,
        InitMode::Lxd => Mode::Lxd,
    }
}

pub fn parse_node_flag(flag: &str) -> Result<(String, Option<String>)> {
    let (name, target) = match flag.split_once(':') {
        Some((n, t)) => (n.to_string(), non_empty(t.to_string())),
        None => (flag.to_string(), None),
    };
    if !collocate_cluster::preseed::valid_instance_name(&name) {
        return Err(Error::Invalid(format!("node name {name:?} is not a valid LXD instance name")));
    }
    Ok((name, target))
}

pub fn default_nodes(count: u32, members: &[String], prefix: &str) -> BTreeMap<String, NodeRecord> {
    (0..count as usize)
        .map(|i| {
            let target = (members.len() > 1).then(|| members[i % members.len()].clone());
            (format!("{prefix}-{}", i + 1), NodeRecord { target, ..NodeRecord::default() })
        })
        .collect()
}

fn wants_cluster(a: &InitArgs) -> bool {
    a.lxd_remote.is_some()
        || a.project.is_some()
        || a.nodes.is_some()
        || !a.node.is_empty()
        || a.image.is_some()
        || a.node_cpus.is_some()
        || a.node_memory.is_some()
        || a.install.is_some()
}

pub fn apply_flags(mut p: Preseed, a: &InitArgs) -> Result<Preseed> {
    if let Some(m) = a.mode {
        p.mode = mode_of(m);
    }
    let d = &mut p.daemon;
    if let Some(v) = &a.subnet {
        d.subnet = v.clone();
    }
    if let Some(v) = &a.bridge {
        d.bridge = v.clone();
    }
    if let Some(v) = &a.root_mode {
        d.root_mode = RootModeSetting::parse(v)?;
    }
    if let Some(v) = &a.group {
        d.group = v.clone();
    }
    if let Some(v) = &a.node_name {
        d.node_name = Some(v.clone());
    }
    if let Some(v) = &a.memory {
        d.defaults.memory = Some(v.clone());
    }
    if let Some(v) = a.cpus {
        d.defaults.cpus = Some(v);
    }
    if let Some(v) = a.pids_max {
        d.defaults.pids_max = Some(v);
    }
    if let Some(v) = &a.https_address {
        d.https_address = non_empty(v.clone());
    }
    if p.mode == Mode::Lxd || wants_cluster(a) {
        let c = p.cluster.get_or_insert_with(ClusterPreseed::default);
        if let Some(v) = &a.lxd_remote {
            c.remote = Some(v.clone());
        }
        if let Some(v) = &a.lxd_url {
            c.url = Some(v.clone());
        }
        if let Some(v) = &a.lxd_token {
            c.token = Some(v.clone());
        }
        if let Some(v) = &a.project {
            c.project = Some(v.clone());
        }
        if let Some(v) = &a.image {
            c.image = v.clone();
        }
        if let Some(v) = &a.install {
            Install::parse(v)?;
            c.install = v.clone();
        }
        if !a.node.is_empty() {
            c.nodes.clear();
            for flag in &a.node {
                let (name, target) = parse_node_flag(flag)?;
                c.nodes.insert(name, NodeRecord { target, ..NodeRecord::default() });
            }
        } else if let Some(n) = a.nodes {
            c.nodes = default_nodes(n, &[], NODE_PREFIX);
        }
        for rec in c.nodes.values_mut() {
            if rec.cpus.is_none() {
                rec.cpus = a.node_cpus;
            }
            if rec.memory.is_none() {
                rec.memory = a.node_memory.clone();
            }
        }
    }
    Ok(p)
}

pub fn fill_targets(c: &mut ClusterPreseed, members: &[String], count: Option<u32>) {
    if c.nodes.is_empty() {
        let n = count.unwrap_or_else(|| members.len().max(1) as u32);
        c.nodes = default_nodes(n, members, NODE_PREFIX);
        return;
    }
    if members.len() > 1 {
        for (i, rec) in c.nodes.values_mut().enumerate() {
            if rec.target.is_none() {
                rec.target = Some(members[i % members.len()].clone());
            }
        }
    }
}

pub fn interactive(
    p: &mut dyn Prompter,
    mut preseed: Preseed,
    members: &dyn Fn(&ClusterPreseed) -> Result<Vec<String>>,
) -> Result<Preseed> {
    let current = match preseed.mode {
        Mode::Local => "local",
        Mode::Lxd => "lxd",
    };
    preseed.mode = ask_parsed(p, "Where should collocate run containers? [local/lxd]", current, parse_mode)?;
    if preseed.mode == Mode::Lxd {
        let mut c = preseed.cluster.take().unwrap_or_default();
        c.remote = non_empty(p.ask("LXD remote to use, empty for the local LXD", c.remote.as_deref().unwrap_or(""))?);
        if c.remote.is_some() {
            c.url = non_empty(p.ask("Address of the remote, empty if it is already added", c.url.as_deref().unwrap_or(""))?);
            if c.url.is_some() {
                c.token = non_empty(p.ask("Trust token for the remote", "")?);
            }
        }
        c.project = non_empty(p.ask("LXD project", c.project.as_deref().unwrap_or("default"))?).filter(|v| v != "default");
        let found = members(&c)?;
        let default_count = if found.len() > 1 { found.len() } else { c.nodes.len().max(1) };
        let count = ask_parsed(p, "How many nodes?", &default_count.to_string(), parse_count)?;
        let prefix = ask_parsed(p, "Name prefix for the nodes", NODE_PREFIX, |s| {
            let s = s.trim().to_string();
            if collocate_cluster::preseed::valid_instance_name(&format!("{s}-1")) {
                Ok(s)
            } else {
                Err(Error::Invalid(format!("{s:?} does not make valid instance names")))
            }
        })?;
        c.nodes = default_nodes(count, &found, &prefix);
        c.image = p.ask("Image for the nodes", &c.image)?;
        let cpus = ask_parsed(p, "CPU limit per node, empty for none", "", parse_optional_cpus)?;
        let memory = ask_parsed(p, "Memory limit per node, empty for none", "", parse_optional_size)?;
        for rec in c.nodes.values_mut() {
            rec.cpus = cpus;
            rec.memory = memory.clone();
        }
        let current_install = c.install.clone();
        c.install =
            ask_parsed(p, "How should collocate be installed on the nodes? [channel:NAME, file:PATH, deb:PATH]", &current_install, |s| {
                Install::parse(s.trim()).map(|i| i.label())
            })?;
        preseed.cluster = Some(c);
        eprintln!("Settings for the collocate daemon on each node:");
    }
    let d = &mut preseed.daemon;
    d.subnet = ask_parsed(p, "Container subnet", &d.subnet.clone(), |s| {
        let v = s.trim().to_string();
        collocate_net::ipam::Subnet::parse(&v)?;
        Ok(v)
    })?;
    d.bridge = ask_parsed(p, "Bridge name", &d.bridge.clone(), |s| {
        let probe = DaemonSettings { bridge: s.trim().to_string(), ..DaemonSettings::default() };
        probe.validate().map(|_| probe.bridge)
    })?;
    d.root_mode = ask_parsed(p, "Root filesystem mode [auto/overlay/fuse-overlay/bind-ro]", d.root_mode.label(), |s| {
        RootModeSetting::parse(s.trim())
    })?;
    d.group = ask_parsed(p, "Group allowed to use collocate", &d.group.clone(), |s| {
        let probe = DaemonSettings { group: s.trim().to_string(), ..DaemonSettings::default() };
        probe.validate().map(|_| probe.group)
    })?;
    let mem_default = d.defaults.memory.clone().unwrap_or_default();
    d.defaults.memory = ask_parsed(p, "Default memory limit per container, empty for none", &mem_default, parse_optional_size)?;
    let https_default = d.https_address.clone().unwrap_or_default();
    d.https_address = ask_parsed(p, "Address for remote clients, empty to disable (for example :8443)", &https_default, |s| {
        let v = non_empty(s.to_string());
        if let Some(a) = &v {
            collocate_core::settings::parse_listen_address(a)?;
        }
        Ok(v)
    })?;
    preseed.validate()?;
    Ok(preseed)
}

fn info_json(api: &mut dyn Api) -> Result<serde_json::Value> {
    match api.call(Request::Info)? {
        Response::Text { text } => Ok(serde_json::from_str(&text)?),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn info_with_retry(cli: &Cli, deadline: Instant) -> Result<serde_json::Value> {
    loop {
        match crate::transport::api(cli).and_then(|mut a| info_json(a.as_mut())) {
            Ok(v) => return Ok(v),
            Err(Error::Unreachable(m)) if m.contains("Permission denied") => return Err(socket_hint(Error::Unreachable(m))),
            Err(e) if !matches!(e, Error::Unreachable(_) | Error::Eof | Error::Io(_) | Error::Timeout(_)) => return Err(e),
            Err(e) if Instant::now() >= deadline => {
                return Err(Error::Unreachable(format!("{}; is the collocate daemon running? (snap services collocate)", e.payload())))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

pub fn missing_plugs(info: &serde_json::Value) -> Vec<String> {
    info["interfaces"]
        .as_array()
        .map(|a| {
            a.iter().filter(|i| i["connected"].as_bool() == Some(false)).filter_map(|i| i["plug"].as_str().map(String::from)).collect()
        })
        .unwrap_or_default()
}

pub fn failed_checks(info: &serde_json::Value) -> Vec<String> {
    info["checks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| c["ok"].as_bool() == Some(false))
                .filter(|c| c["name"].as_str().is_some_and(|n| FATAL_CHECKS.contains(&n)))
                .map(|c| format!("{}: {}", c["name"].as_str().unwrap_or(""), c["detail"].as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default()
}

fn group_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/group").map(|t| t.lines().any(|l| l.split(':').next() == Some(name))).unwrap_or(false)
}

fn settings_applied(info: &serde_json::Value, settings: &DaemonSettings) -> bool {
    info["initialized"].as_bool() == Some(true)
        && serde_json::from_value::<DaemonSettings>(info["settings"].clone()).is_ok_and(|s| &s == settings)
}

pub fn apply_local(cli: &Cli, settings: &DaemonSettings, force: bool, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let info = info_with_retry(cli, deadline)?;
    let missing = missing_plugs(&info);
    if !missing.is_empty() {
        return Err(Error::Invalid(format!(
            "the collocate snap needs these interfaces connected first:\n  {}",
            connect_commands("collocate", &missing).join("\n  ")
        )));
    }
    let failed = failed_checks(&info);
    if !failed.is_empty() {
        return Err(Error::Invalid(format!("this machine cannot run collocate containers: {}", failed.join("; "))));
    }
    narrate_detail(cli, "Preflight checks passed.");
    crate::transport::call(cli, Request::Init { settings: settings.clone(), force })?;
    loop {
        if let Ok(info) = crate::transport::api(cli).and_then(|mut a| info_json(a.as_mut())) {
            if settings_applied(&info, settings) {
                break;
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Timeout("the daemon did not come back with the new settings".into()));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !crate::transport::is_remote(cli)? && !group_exists(&settings.group) {
        eprintln!(
            "note: group {g} does not exist, so only root can use collocate. Create it with:\n  sudo groupadd --system {g} && sudo usermod -aG {g} $USER",
            g = settings.group
        );
    }
    Ok(())
}

pub fn ensure_remote(lxc: &Lxc, c: &ClusterPreseed) -> Result<()> {
    let (Some(remote), Some(url)) = (&c.remote, &c.url) else { return Ok(()) };
    let list = lxc.run(&["remote".into(), "list".into(), "--format".into(), "json".into()], None)?;
    let known =
        serde_json::from_str::<serde_json::Value>(&list).ok().and_then(|v| v.as_object().map(|o| o.contains_key(remote))).unwrap_or(false);
    if known {
        return Ok(());
    }
    let mut args = vec!["remote".to_string(), "add".into(), remote.clone(), url.clone(), "--accept-certificate".into()];
    if let Some(t) = &c.token {
        args.push("--token".into());
        args.push(t.clone());
    }
    lxc.run(&args, None).map(|_| ())
}

pub fn lxd_members(lxc: &Lxc, target: &LxdTarget) -> Result<Vec<String>> {
    let query = vec!["query".to_string(), format!("{}/1.0", target.scope())];
    let server = lxc.run(&query, None).map_err(|e| {
        Error::Unreachable(format!("cannot reach LXD ({e}); if collocate is a snap, run: sudo snap connect collocate:lxd lxd:lxd"))
    })?;
    let clustered = serde_json::from_str::<serde_json::Value>(&server)
        .ok()
        .and_then(|v| v["environment"]["server_clustered"].as_bool())
        .unwrap_or(false);
    if !clustered {
        return Ok(Vec::new());
    }
    let out = lxc.run(&["cluster".into(), "list".into(), target.scope(), "--format".into(), "json".into()], None)?;
    Ok(cluster_members(&out))
}

pub fn observe(lxc: &Lxc, target: &LxdTarget, nodes: &BTreeMap<String, NodeRecord>) -> Result<Observed> {
    let mut o = Observed { profile: lxc.ok(&provision::profile_show_args(target)), ..Observed::default() };
    let states = provision::instance_states(&lxc.run(&provision::list_args(target), None)?)?;
    for (name, status) in states {
        if !nodes.contains_key(&name) {
            continue;
        }
        if status == "Running" && lxc.ok(&provision::installed_args(target, &name)) {
            o.installed.insert(name.clone());
            if lxc.run(&provision::info_args(target, &name), None).is_ok_and(|t| provision::reports_initialized(&t)) {
                o.initialized.insert(name.clone());
            }
        }
        o.instances.insert(name, status);
    }
    Ok(o)
}

pub fn node_api(lxc: &Lxc, target: &LxdTarget, node: &str, relay: &str) -> Result<LxcApi> {
    LxcApi::spawn_args(&lxc.program, &provision::exec(target, node, vec![relay.to_string()]))
}

pub fn snapd_hint(node: &str) -> String {
    format!(
        "snapd is not working on {node}, which happens when the node is itself nested in a container that cannot load AppArmor profiles; install from packages instead with --install deb:collocated.deb,collocate.deb"
    )
}

pub fn unreadable_hint(path: &str, err: &std::io::Error, snap: bool) -> String {
    if snap && err.kind() == std::io::ErrorKind::PermissionDenied {
        format!("cannot read {path}: {err}; connect the home-all interface with 'sudo snap connect collocate:home-all', or move the file under /media or /mnt")
    } else {
        format!("cannot read {path}: {err}")
    }
}

pub fn check_install_files(install: &Install, snap: bool) -> Result<()> {
    for path in install.files() {
        if let Err(e) = std::fs::File::open(&path) {
            return Err(Error::Invalid(unreadable_hint(&path, &e, snap)));
        }
    }
    Ok(())
}

pub fn provision_nodes(
    cli: &Cli,
    lxc: &Lxc,
    target: &LxdTarget,
    nodes: &BTreeMap<String, NodeRecord>,
    image: &str,
    install: &Install,
    settings: &DaemonSettings,
) -> Result<()> {
    check_install_files(install, Layout::detect().snap)?;
    let observed = observe(lxc, target, nodes)?;
    for step in provision::plan(target, nodes, image, install, settings, &observed)? {
        narrate(cli, &format!("{}...", step.describe()));
        narrate_detail(cli, &format!("{} {}", lxc.program, step.args.join(" ")));
        if let Err(e) = lxc.run(&step.args, step.stdin.as_deref()) {
            let snap_step = matches!(step.kind, StepKind::WaitReady | StepKind::Install | StepKind::Connect);
            if install.is_snap() && snap_step && !lxc.ok(&provision::exec(target, &step.node, vec!["snap".into(), "list".into()])) {
                return Err(Error::Internal(format!("{}\n{}", e.payload(), snapd_hint(&step.node))));
            }
            return Err(e);
        }
    }
    for node in nodes.keys() {
        let mut api = node_api(lxc, target, node, install.relay())?;
        let info = info_json(&mut api)?;
        if info["initialized"].as_bool() != Some(true) {
            return Err(Error::Internal(format!("node {node} did not report itself initialized")));
        }
    }
    Ok(())
}

pub fn merge_registry(existing: Option<Registry>, fresh: Registry) -> Registry {
    match existing {
        Some(mut r) if r.remote == fresh.remote && r.project == fresh.project => {
            r.image = fresh.image;
            r.install = fresh.install;
            r.daemon = fresh.daemon;
            r.nodes.extend(fresh.nodes);
            r
        }
        _ => fresh,
    }
}

fn apply_lxd(cli: &Cli, a: &InitArgs, preseed: &Preseed, lxc: &Lxc) -> Result<()> {
    let c = preseed.cluster.as_ref().ok_or_else(|| Error::Invalid("mode lxd needs a cluster section".into()))?;
    let target = c.target();
    let install = Install::parse(&c.install)?;
    provision_nodes(cli, lxc, &target, &c.nodes, &c.image, &install, &preseed.daemon)?;
    let layout = Layout::detect();
    let registry = merge_registry(Registry::load(&layout.cluster_registry)?, Registry::from_preseed(c, &preseed.daemon));
    registry.save(&layout.cluster_registry)?;
    narrate_detail(cli, &format!("Saved the cluster registry to {}.", layout.cluster_registry.display()));
    if a.force {
        narrate_detail(cli, "--force has no effect on nodes that are already initialized with other settings.");
    }
    Ok(())
}

fn dump(cli: &Cli) -> Result<i32> {
    let layout = Layout::detect();
    let preseed = match Registry::load(&layout.cluster_registry)? {
        Some(r) => Preseed {
            mode: Mode::Lxd,
            daemon: r.daemon,
            cluster: Some(ClusterPreseed {
                remote: r.remote,
                project: r.project,
                image: r.image,
                install: r.install,
                nodes: r.nodes,
                ..ClusterPreseed::default()
            }),
        },
        None => {
            let info = info_json(crate::transport::api(cli)?.as_mut())?;
            if info["initialized"].as_bool() == Some(false) {
                return Err(Error::NotInitialized("collocate is not initialized; there is nothing to dump".into()));
            }
            let daemon = serde_json::from_value(info["settings"].clone())
                .map_err(|e| Error::Internal(format!("the daemon did not report its settings: {e}")))?;
            Preseed { mode: Mode::Local, daemon, cluster: None }
        }
    };
    if cli.format == Format::Json {
        json(&preseed);
    } else {
        print!("{}", preseed.to_yaml()?);
    }
    Ok(0)
}

pub fn init(cli: &Cli, a: &InitArgs) -> Result<i32> {
    if a.dump {
        return dump(cli);
    }
    let layout = Layout::detect();
    let lxc = Lxc::new(a.lxc.clone().unwrap_or(layout.lxc));
    let base = if a.preseed { Preseed::parse(&read_stdin()?)? } else { Preseed::default() };
    let mut preseed = apply_flags(base, a)?;
    if !a.auto && !a.preseed {
        if !std::io::stdin().is_terminal() {
            return Err(Error::Invalid("collocate init is interactive; pass --auto or --preseed to run it without a terminal".into()));
        }
        let members = |c: &ClusterPreseed| -> Result<Vec<String>> {
            ensure_remote(&lxc, c)?;
            lxd_members(&lxc, &c.target())
        };
        preseed = interactive(&mut TerminalPrompter, preseed, &members)?;
        if TerminalPrompter.ask("Print the preseed for this configuration? [yes/no]", "no")?.to_ascii_lowercase().starts_with('y') {
            print!("{}", preseed.to_yaml()?);
        }
    } else if preseed.mode == Mode::Lxd {
        let c = preseed.cluster.get_or_insert_with(ClusterPreseed::default);
        ensure_remote(&lxc, c)?;
        let members = lxd_members(&lxc, &c.target())?;
        fill_targets(c, &members, a.nodes);
    }
    preseed.validate()?;
    if preseed.mode == Mode::Lxd && crate::transport::is_remote(cli)? {
        return Err(Error::Invalid("the LXD mode drives LXD from this machine; run it without --remote".into()));
    }
    let timeout = Duration::from_secs(a.timeout.max(1));
    match preseed.mode {
        Mode::Local => {
            apply_local(cli, &preseed.daemon, a.force, timeout)?;
            narrate(cli, &success_line("Initialized", "collocate"));
        }
        Mode::Lxd => {
            if let Some(c) = &preseed.cluster {
                ensure_remote(&lxc, c)?;
            }
            apply_lxd(cli, a, &preseed, &lxc)?;
            let n = preseed.cluster.as_ref().map(|c| c.nodes.len()).unwrap_or(0);
            narrate(cli, &success_line("Initialized", &format!("collocate on {n} LXD node{}", if n == 1 { "" } else { "s" })));
        }
    }
    Ok(0)
}
