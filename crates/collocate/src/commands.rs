use crate::cli::{Cli, ClusterCmd, Command, ComposeArgs, ImageCmd, LoadBalancerArgs, LoadBalancerCmd, NodeCmd, SecretCmd};
use crate::output::{self, confirm, is_interactive, success_line, terminal_width, Format, Verbosity};
use crate::runspec::build_spec;
use crate::status::{format_ports, format_process, format_uptime, read_listening, read_procs, read_uptime};
use crate::table::{empty_state, render_with, TableOptions};
use collocate_cluster::lxc::Lxc;
use collocate_cluster::preseed::{LxdTarget, NodeRecord, Registry};
use collocate_cluster::provision::Install;
use collocate_compose::build::signal_number;
use collocate_compose::convert::convert_docker_compose;
use collocate_compose::model::ComposeFile;
use collocate_compose::up::{down, plan_only, up, UpOptions};
use collocate_core::client::Client;
use collocate_core::layout::Layout;
use collocate_core::net::{Algorithm, NoBackends, Proto};
use collocate_core::request::{ContainerInfo, LbSpec, LbStatus, LogSource, Request, Response, State};
use collocate_core::spec::{ImageKind, Series};
use collocate_core::{Error, Result};
use collocate_image::config::ImageMeta;
use collocate_image::pull::PullPolicy;
use collocate_sys::fdpass::SendWithFds;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn call(cli: &Cli, req: Request) -> Result<Response> {
    crate::transport::call(cli, req)
}

pub(crate) use crate::transport::socket_hint;

fn host_series() -> Series {
    let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    text.lines().find_map(|l| l.strip_prefix("VERSION_ID=")).and_then(|v| Series::parse(v.trim_matches('"')).ok()).unwrap_or(Series::Noble)
}

fn containers(cli: &Cli, all: bool, project: Option<String>) -> Result<Vec<ContainerInfo>> {
    match call(cli, Request::Ps { all, project })? {
        Response::Containers(c) => Ok(c),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn state_word(s: State) -> &'static str {
    match s {
        State::Running => "running",
        State::Stopped => "stopped",
        State::Starting => "starting",
        State::Exited => "exited",
    }
}

pub(crate) fn json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub(crate) fn narrate(cli: &Cli, message: &str) {
    output::narrate(cli.verbosity(), Verbosity::Brief, message);
}

pub(crate) fn narrate_detail(cli: &Cli, message: &str) {
    output::narrate(cli.verbosity(), Verbosity::Verbose, message);
}

pub(crate) fn abort() -> i32 {
    eprintln!("Aborted.");
    1
}

pub(crate) fn confirm_action(cli: &Cli, prompt: &str) -> Result<bool> {
    confirm(prompt, false, cli.yes)
}

fn table_opts(columns: &[String], no_headers: bool, no_truncate: bool) -> TableOptions {
    TableOptions { columns: columns.to_vec(), no_headers, no_truncate, interactive: is_interactive(), max_width: terminal_width(80) }
}

fn follow_logs(cli: &Cli, target: &str, until_exit: bool, tail: Option<usize>, services: &[String], raw: bool) -> Result<()> {
    let first = LogSource::from_flags(raw, services);
    let (mut offset, source) =
        match call(cli, Request::Logs { target: target.into(), tail, offset: None, source: first, services: services.to_vec() })? {
            Response::Log { data, next_offset, source } => {
                print!("{data}");
                (next_offset, source)
            }
            _ => (0, first),
        };
    loop {
        let running = containers(cli, true, None)?
            .iter()
            .any(|c| (c.name == target || c.id.to_string().starts_with(target)) && c.state == State::Running);
        if source == LogSource::Pebble && !running {
            return Ok(());
        }
        let req = Request::Logs { target: target.into(), tail: None, offset: Some(offset), source, services: services.to_vec() };
        if let Response::Log { data, next_offset, .. } = call(cli, req)? {
            print!("{data}");
            let _ = std::io::stdout().flush();
            offset = next_offset;
        }
        if !running || !until_exit {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_status(cli: &Cli, target: &str) -> Result<i32> {
    match call(cli, Request::Wait { target: target.into() })? {
        Response::Exit { status } => Ok(status),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

pub(crate) fn read_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s.trim_end_matches(['\n', '\r']).to_string())
}

fn daemon_subnet(cli: &Cli) -> Result<String> {
    match call(cli, Request::Info)? {
        Response::Text { text } => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["subnet"].as_str().map(String::from))
            .ok_or_else(|| Error::Internal("daemon did not report its subnet".into())),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn compose_options(cli: &Cli, a: &ComposeArgs, file: &Path) -> Result<UpOptions> {
    let subnet = match &a.subnet {
        Some(s) => s.clone(),
        None => daemon_subnet(cli)?,
    };
    Ok(UpOptions {
        subnet,
        base_dir: file.parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf),
        regenerate_secrets: a.regenerate_secrets.as_ref().map(|s| if s.is_empty() { Vec::new() } else { vec![s.clone()] }),
        dry_run: a.dry_run,
        ready_timeout: Duration::from_secs(a.timeout),
        poll_interval: Duration::from_millis(250),
    })
}

fn status_rows(cli: &Cli, target: Option<&str>) -> Result<Vec<Vec<String>>> {
    let cgroup_root = std::env::var("COLLOCATE_CGROUP_ROOT").unwrap_or_else(|_| "/sys/fs/cgroup/collocate.slice/containers".into());
    let mut rows = Vec::new();
    for c in containers(cli, true, None)? {
        if target.is_some_and(|t| c.name != t && !c.id.to_string().starts_with(t)) {
            continue;
        }
        let procs = read_procs(&Path::new(&cgroup_root).join(c.id.to_string()));
        let main_pid = c.pid.or_else(|| procs.first().map(|p| p.pid));
        let (tcp, udp) = main_pid.map(read_listening).unwrap_or_default();
        rows.push(vec![
            c.name.clone(),
            c.project.clone().unwrap_or_else(|| "—".into()),
            c.series.clone().or_else(|| c.image_kind.map(|k| k.label().to_string())).unwrap_or_else(|| "—".into()),
            state_word(c.state).to_string(),
            if c.state == State::Running { format_process(&procs) } else { "—".into() },
            if c.state == State::Running { format_ports(&tcp, &udp) } else { "—".into() },
            if c.published.is_empty() { "—".into() } else { c.published.join(",") },
            c.address.map_or_else(|| "—".into(), |a| a.to_string()),
            match (c.state, main_pid.and_then(read_uptime)) {
                (State::Running, Some(u)) => format_uptime(u),
                _ => "—".into(),
            },
        ]);
    }
    Ok(rows)
}

const STATUS_HEADERS: [&str; 9] = ["NAME", "PROJECT", "SERIES", "STATE", "PROCESS", "LISTENING", "PUBLISHED", "ADDRESS", "UPTIME"];

fn describe_image(m: &ImageMeta) -> String {
    format!(
        "name: {}\nkind: {}\ndigest: {}\nlayers: {}\nentrypoint: {:?}\ncmd: {:?}\nuser: {}\nworkdir: {}",
        m.name,
        match m.kind {
            ImageKind::Pebble => "rock (pebble)",
            ImageKind::Oci => "oci",
        },
        m.digest,
        m.layers.len(),
        m.config.entrypoint,
        m.config.cmd,
        m.config.user,
        m.config.working_dir
    )
}

fn describe_load_balancer(l: &LbStatus) -> String {
    format!(
        "name: {}/{}\nvip: {}\nlisten: {}\nbackend: {} (port {})\nbackends: {}",
        l.spec.project,
        l.spec.name,
        l.vip,
        l.spec.listen,
        l.spec.backend_service,
        l.spec.backend_port,
        if l.backends.is_empty() { "—".to_string() } else { l.backends.join(", ") }
    )
}

fn parse_proto(s: &str) -> Result<Proto> {
    match s.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Proto::Tcp),
        "udp" => Ok(Proto::Udp),
        other => Err(Error::Invalid(format!("unknown protocol {other}"))),
    }
}

fn parse_algorithm(s: &str) -> Result<Algorithm> {
    match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "round-robin" => Ok(Algorithm::RoundRobin),
        "random" => Ok(Algorithm::Random),
        "source-hash" => Ok(Algorithm::SourceHash),
        other => Err(Error::Invalid(format!("unknown algorithm {other}"))),
    }
}

fn parse_on_no_backends(s: &str) -> Result<NoBackends> {
    match s.to_ascii_lowercase().as_str() {
        "reject" => Ok(NoBackends::Reject),
        "drop" => Ok(NoBackends::Drop),
        other => Err(Error::Invalid(format!("unknown on-no-backends policy {other}"))),
    }
}

fn build_lb_spec(a: &LoadBalancerArgs) -> Result<LbSpec> {
    let vip = a.vip.as_ref().map(|v| v.parse().map_err(|_| Error::Invalid(format!("invalid vip address {v}")))).transpose()?;
    Ok(LbSpec {
        project: a.project.clone(),
        name: a.name.clone(),
        proto: parse_proto(&a.proto)?,
        listen: a.listen,
        publish: a.publish.clone(),
        backend_service: a.backend_service.clone(),
        backend_port: a.backend_port,
        algorithm: parse_algorithm(&a.algorithm)?,
        on_no_backends: parse_on_no_backends(&a.on_no_backends)?,
        drain_secs: a.drain,
        vip,
    })
}

pub fn run(cli: &Cli) -> Result<i32> {
    match &cli.command {
        Command::Run(a) => {
            let spec = build_spec(a, host_series(), &|k| std::env::var(k).ok(), &|p| Ok(std::fs::read_to_string(p)?), &|n| {
                lookup_or_pull(cli, n)
            })?;
            let id = match call(cli, Request::Run(Box::new(spec)))? {
                Response::Id { id } => id,
                other => return Err(Error::Internal(format!("unexpected response {other:?}"))),
            };
            if a.detach {
                println!("{id}");
                narrate(cli, &success_line("Started", &id.to_string()));
                return Ok(0);
            }
            let target = id.to_string();
            follow_logs(cli, &target, true, None, &[], true)?;
            wait_status(cli, &target)
        }
        Command::List { all, project, columns, no_headers, no_truncate } => {
            let list = containers(cli, *all, project.clone())?;
            if cli.format == Format::Json {
                json(&list);
            } else if list.is_empty() {
                print!("{}", empty_state("No containers found."));
            } else {
                let rows: Vec<Vec<String>> = list
                    .iter()
                    .map(|c| {
                        vec![
                            c.id.short(),
                            c.name.clone(),
                            state_word(c.state).into(),
                            c.address.map_or_else(|| "—".into(), |a| a.to_string()),
                            c.project.clone().unwrap_or_else(|| "—".into()),
                        ]
                    })
                    .collect();
                print!(
                    "{}",
                    render_with(&["ID", "NAME", "STATE", "ADDRESS", "PROJECT"], &rows, &table_opts(columns, *no_headers, *no_truncate))
                );
            }
            Ok(0)
        }
        Command::Status { target, watch, json: as_json, tree: _, columns, no_headers, no_truncate } => loop {
            let rows = status_rows(cli, target.as_deref())?;
            if *as_json || cli.format == Format::Json {
                let objs: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        STATUS_HEADERS
                            .iter()
                            .zip(r)
                            .map(|(h, v)| (h.to_lowercase(), serde_json::Value::String(v.clone())))
                            .collect::<serde_json::Map<_, _>>()
                            .into()
                    })
                    .collect();
                json(&objs);
            } else if rows.is_empty() {
                print!("{}", empty_state("No containers found."));
            } else {
                print!("{}", render_with(&STATUS_HEADERS, &rows, &table_opts(columns, *no_headers, *no_truncate)));
            }
            match watch {
                Some(secs) => {
                    std::thread::sleep(Duration::from_secs((*secs).max(1)));
                    print!("\x1b[2J\x1b[H");
                }
                None => return Ok(0),
            }
        },
        Command::Stop { targets, timeout } => {
            for t in targets {
                call(cli, Request::Stop { target: t.clone(), timeout_secs: *timeout })?;
                narrate(cli, &success_line("Stopped", t));
            }
            Ok(0)
        }
        Command::Kill { targets, signal } => {
            let sig = signal_number(signal)?;
            for t in targets {
                call(cli, Request::Kill { target: t.clone(), signal: sig })?;
                narrate(cli, &success_line("Signaled", t));
            }
            Ok(0)
        }
        Command::Delete { targets, force } => {
            let subject = if targets.len() == 1 {
                format!("container {}", targets[0])
            } else {
                format!("{} containers ({})", targets.len(), targets.join(", "))
            };
            if !confirm_action(cli, &format!("Delete {subject}?"))? {
                return Ok(abort());
            }
            for t in targets {
                call(cli, Request::Rm { target: t.clone(), force: *force, keep_data: false })?;
                narrate(cli, &success_line("Deleted", t));
            }
            Ok(0)
        }
        Command::Start { targets } => {
            for t in targets {
                call(cli, Request::Start { target: t.clone() })?;
                narrate(cli, &success_line("Started", t));
            }
            Ok(0)
        }
        Command::Restart { targets, timeout } => {
            for t in targets {
                call(cli, Request::Stop { target: t.clone(), timeout_secs: *timeout })?;
                call(cli, Request::Start { target: t.clone() })?;
                narrate(cli, &success_line("Restarted", t));
            }
            Ok(0)
        }
        Command::Wait { target } => {
            let code = wait_status(cli, target)?;
            println!("{code}");
            Ok(0)
        }
        Command::Logs { target, follow, tail, services, raw } => {
            follow_logs(cli, target, *follow, *tail, services, *raw)?;
            Ok(0)
        }
        Command::Exec { env, user, workdir, timeout, service, target, command } => {
            let envs = env.iter().filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
            let req = Request::Exec {
                target: target.clone(),
                argv: command.clone(),
                env: envs,
                user: user.clone(),
                workdir: workdir.clone(),
                tty: false,
                timeout_secs: *timeout,
                service: service.clone(),
            };
            if let Some(status) = crate::transport::with_remote_api(cli, |api| {
                api.exec(&req, Box::new(std::io::stdin()), &mut std::io::stdout(), &mut std::io::stderr())
            })? {
                return Ok(status);
            }
            let mut c = crate::transport::local(cli)?;
            let (i, o, e) = (std::io::stdin(), std::io::stdout(), std::io::stderr());
            c.send_with_fds(&req, &[&i.as_raw_fd(), &o.as_raw_fd(), &e.as_raw_fd()])?;
            match c.read_response()? {
                Response::Exit { status } => Ok(status),
                other => Err(Error::Internal(format!("unexpected response {other:?}"))),
            }
        }
        Command::Cp { src, dst } => cp(cli, src, dst),
        Command::Commit { target, image } => match call(cli, Request::Commit { target: target.clone(), image: image.clone() })? {
            Response::Text { text } => {
                narrate(cli, &success_line("Committed", &format!("{image} ({text})")));
                if cli.format == Format::Json {
                    json(&text);
                } else {
                    println!("{text}");
                }
                Ok(0)
            }
            other => Err(Error::Internal(format!("unexpected response {other:?}"))),
        },
        Command::Secret(cmd) => secret(cli, cmd),
        Command::LoadBalancer(cmd) => load_balancer(cli, cmd),
        Command::Image(cmd) => image(cli, cmd),
        Command::Cluster(cmd) => cluster(cli, cmd),
        Command::Node(cmd) => node(cli, cmd),
        Command::Up(a) => {
            let file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
            let mut c = crate::transport::api(cli)?;
            if a.managed && !file.configs.is_empty() {
                return Err(Error::Invalid(
                    "--managed does not support configs templates yet; the controller reads the file from its own directory".into(),
                ));
            }
            let report = up(c.as_mut(), &file, &compose_options(cli, a, &a.file)?)?;
            if a.managed {
                call(cli, Request::ControllerSet { compose: std::fs::read_to_string(&a.file)? })?;
                narrate(cli, &format!("The controller now manages project {}.", file.project));
            }
            if cli.format == Format::Json {
                json(&report);
            } else {
                for (label, names) in [
                    ("Created", &report.created),
                    ("Recreated", &report.recreated),
                    ("Started", &report.started),
                    ("Removed", &report.removed),
                ] {
                    for n in names {
                        narrate(cli, &success_line(label, n));
                    }
                }
                for n in &report.kept {
                    narrate_detail(cli, &format!("{n} is already up to date."));
                }
            }
            Ok(0)
        }
        Command::Plan(a) => {
            let file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
            let mut c = crate::transport::api(cli)?;
            let p = plan_only(c.as_mut(), &file, &compose_options(cli, a, &a.file)?)?;
            if cli.format == Format::Json {
                json(&p);
            } else {
                for (label, names) in
                    [("create", &p.create), ("recreate", &p.recreate), ("start", &p.start), ("remove", &p.remove), ("keep", &p.keep)]
                {
                    for n in names {
                        println!("{label:<10} {n}");
                    }
                }
            }
            Ok(0)
        }
        Command::Down(a) => {
            let file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
            if !confirm_action(cli, &format!("Stop and remove every container for project {}?", file.project))? {
                return Ok(abort());
            }
            let mut c = crate::transport::api(cli)?;
            let removed = down(c.as_mut(), &file, &compose_options(cli, a, &a.file)?)?;
            if cli.format == Format::Json {
                json(&removed);
            } else if removed.is_empty() {
                narrate_detail(cli, "Nothing to remove.");
            } else {
                for n in &removed {
                    narrate(cli, &success_line("Removed", n));
                }
            }
            Ok(0)
        }
        Command::Config { file, from_docker_compose, project } => {
            match from_docker_compose {
                Some(src) => {
                    let base =
                        src.parent().filter(|p| !p.as_os_str().is_empty()).map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
                    let conv =
                        convert_docker_compose(&std::fs::read_to_string(src)?, project, &std::fs::canonicalize(&base).unwrap_or(base))?;
                    println!("{}", serde_saphyr::to_string(&conv.file).map_err(|e| Error::Internal(e.to_string()))?);
                    for m in &conv.report.unsupported {
                        eprintln!("unsupported: {m}");
                    }
                    for m in &conv.report.approximated {
                        eprintln!("approximated: {m}");
                    }
                }
                None => {
                    let parsed = ComposeFile::load(&std::fs::read_to_string(file)?)?;
                    println!("{}", serde_saphyr::to_string(&parsed).map_err(|e| Error::Internal(e.to_string()))?);
                }
            }
            Ok(0)
        }
        Command::Init(a) => crate::init::init(cli, a),
        Command::Remote(c) => crate::access::remote(cli, c),
        Command::Trust(c) => crate::access::trust(cli, c),
        Command::Info => match call(cli, Request::Info)? {
            Response::Text { text } => {
                if cli.format == Format::Json {
                    println!("{text}");
                } else {
                    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or(text));
                }
                Ok(0)
            }
            other => Err(Error::Internal(format!("unexpected response {other:?}"))),
        },
        Command::Doctor => {
            let local = collocate_sys::probe::probe();
            let mut failed = local.checks.iter().any(|c| !c.ok);
            let daemon_info = match call(cli, Request::Info) {
                Ok(Response::Text { text }) => Ok(serde_json::from_str::<serde_json::Value>(&text).unwrap_or(serde_json::Value::Null)),
                Ok(other) => Err(Error::Internal(format!("unexpected response {other:?}"))),
                Err(e) => Err(e),
            };
            let initialized = daemon_info.as_ref().ok().and_then(|v| v["initialized"].as_bool());
            let missing = daemon_info.as_ref().map(crate::init::missing_plugs).unwrap_or_default();
            if daemon_info.is_err() || initialized == Some(false) || !missing.is_empty() {
                failed = true;
            }
            if cli.format == Format::Json {
                let checks: Vec<serde_json::Value> =
                    local.checks.iter().map(|c| serde_json::json!({"name": c.name, "ok": c.ok, "detail": c.detail})).collect();
                json(&serde_json::json!({
                    "checks": checks,
                    "daemon_reachable": daemon_info.is_ok(),
                    "initialized": initialized,
                    "missing_interfaces": missing,
                }));
            } else {
                for c in &local.checks {
                    println!("{:<20} {}  {}", c.name, if c.ok { "ok " } else { "FAIL" }, c.detail);
                }
                match &daemon_info {
                    Ok(v) if initialized == Some(false) => println!("daemon: reachable, not initialized; run 'sudo collocate init' ({v})"),
                    Ok(v) => println!("daemon: reachable ({v})"),
                    Err(e) => println!("daemon: {e}"),
                }
                for cmd in collocate_core::layout::connect_commands("collocate", &missing) {
                    println!("interface missing: {cmd}");
                }
                if !missing.is_empty() {
                    println!("note: checks above can fail until the missing interfaces are connected");
                }
            }
            Ok(i32::from(failed))
        }
        Command::Completion { shell } => {
            clap_complete::generate(*shell, &mut <Cli as clap::CommandFactory>::command(), "collocate", &mut std::io::stdout());
            Ok(0)
        }
    }
}

fn secret(cli: &Cli, cmd: &SecretCmd) -> Result<i32> {
    match cmd {
        SecretCmd::List { project } => {
            if let Response::Names(names) = call(cli, Request::SecretList { project: project.clone() })? {
                if cli.format == Format::Json {
                    json(&names);
                } else if names.is_empty() {
                    print!("{}", empty_state("No secrets found."));
                } else {
                    for n in names {
                        println!("{n}");
                    }
                }
            }
            Ok(0)
        }
        SecretCmd::Set { project, name, from_file } => {
            let value = match from_file.as_deref() {
                Some(p) if p != Path::new("-") => std::fs::read_to_string(p)?.trim_end_matches(['\n', '\r']).to_string(),
                _ => read_stdin()?,
            };
            call(cli, Request::SecretSet { project: project.clone(), name: name.clone(), value })?;
            narrate(cli, &success_line("Set", &format!("{project}/{name}")));
            Ok(0)
        }
        SecretCmd::Delete { project, name } => {
            if !confirm_action(cli, &format!("Delete secret {project}/{name}?"))? {
                return Ok(abort());
            }
            call(cli, Request::SecretRemove { project: project.clone(), name: name.clone() })?;
            narrate(cli, &success_line("Deleted", &format!("{project}/{name}")));
            Ok(0)
        }
        SecretCmd::Reveal { project, name } => {
            if let Response::Text { text } = call(cli, Request::SecretReveal { project: project.clone(), name: name.clone() })? {
                println!("{text}");
            }
            Ok(0)
        }
    }
}

enum CpEndpoint {
    Local(PathBuf),
    Remote { target: String, path: String },
}

fn parse_cp_endpoint(s: &str) -> CpEndpoint {
    match s.split_once(':') {
        Some((target, path)) if !target.is_empty() && !path.is_empty() => {
            CpEndpoint::Remote { target: target.to_string(), path: path.to_string() }
        }
        _ => CpEndpoint::Local(PathBuf::from(s)),
    }
}

fn cp_exec(cli: &Cli, target: &str, argv: Vec<String>, stdin: &std::fs::File, stdout: &std::fs::File) -> Result<i32> {
    let req = Request::Exec {
        target: target.to_string(),
        argv,
        env: vec![],
        user: None,
        workdir: None,
        tty: false,
        timeout_secs: None,
        service: None,
    };
    if let Some(status) = crate::transport::with_remote_api(cli, |api| {
        let input = stdin.try_clone()?;
        let mut output = stdout.try_clone()?;
        api.exec(&req, Box::new(input), &mut output, &mut std::io::sink())
    })? {
        return Ok(status);
    }
    let mut c = crate::transport::local(cli)?;
    let stderr = std::fs::File::open("/dev/null")?;
    c.send_with_fds(&req, &[&stdin.as_raw_fd(), &stdout.as_raw_fd(), &stderr.as_raw_fd()])?;
    match c.read_response()? {
        Response::Exit { status } => Ok(status),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn cp(cli: &Cli, src: &str, dst: &str) -> Result<i32> {
    match (parse_cp_endpoint(src), parse_cp_endpoint(dst)) {
        (CpEndpoint::Remote { target, path }, CpEndpoint::Local(local)) => {
            let stdin = std::fs::File::open("/dev/null")?;
            let stdout = std::fs::File::create(&local)?;
            cp_exec(cli, &target, vec!["cat".into(), path], &stdin, &stdout)
        }
        (CpEndpoint::Local(local), CpEndpoint::Remote { target, path }) => {
            let stdin = std::fs::File::open(&local)?;
            let stdout = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
            cp_exec(cli, &target, vec!["tee".into(), path], &stdin, &stdout)
        }
        (CpEndpoint::Local(_), CpEndpoint::Local(_)) => {
            Err(Error::Invalid("cp needs one side to reference a container as name:path".into()))
        }
        (CpEndpoint::Remote { .. }, CpEndpoint::Remote { .. }) => Err(Error::Invalid("cp cannot copy between two containers".into())),
    }
}

fn load_balancer(cli: &Cli, cmd: &LoadBalancerCmd) -> Result<i32> {
    match cmd {
        LoadBalancerCmd::List => match call(cli, Request::LbList)? {
            Response::Lbs(list) => {
                if cli.format == Format::Json {
                    json(&list);
                } else if list.is_empty() {
                    print!("{}", empty_state("No load balancers found."));
                } else {
                    let rows: Vec<Vec<String>> = list
                        .iter()
                        .map(|l| {
                            vec![
                                format!("{}/{}", l.spec.project, l.spec.name),
                                l.vip.to_string(),
                                l.spec.listen.to_string(),
                                l.spec.backend_service.clone(),
                                if l.backends.is_empty() { "—".into() } else { l.backends.join(",") },
                            ]
                        })
                        .collect();
                    print!(
                        "{}",
                        render_with(&["LOAD BALANCER", "VIP", "LISTEN", "SERVICE", "BACKENDS"], &rows, &table_opts(&[], false, false))
                    );
                }
                Ok(0)
            }
            other => Err(Error::Internal(format!("unexpected response {other:?}"))),
        },
        LoadBalancerCmd::Show { project, name } => match call(cli, Request::LbList)? {
            Response::Lbs(list) => {
                let found = list
                    .into_iter()
                    .find(|l| &l.spec.project == project && &l.spec.name == name)
                    .ok_or_else(|| Error::NotFound(format!("load balancer {project}/{name}")))?;
                if cli.format == Format::Json {
                    json(&found);
                } else {
                    println!("{}", describe_load_balancer(&found));
                }
                Ok(0)
            }
            other => Err(Error::Internal(format!("unexpected response {other:?}"))),
        },
        LoadBalancerCmd::Create(a) => {
            let lb = build_lb_spec(a)?;
            call(cli, Request::LbSet { lb })?;
            narrate(cli, &success_line("Configured load balancer", &format!("{}/{}", a.project, a.name)));
            Ok(0)
        }
        LoadBalancerCmd::Delete { project, name } => {
            if !confirm_action(cli, &format!("Delete load balancer {project}/{name}?"))? {
                return Ok(abort());
            }
            call(cli, Request::LbRemove { project: project.clone(), name: name.clone() })?;
            narrate(cli, &success_line("Deleted", &format!("{project}/{name}")));
            Ok(0)
        }
    }
}

fn image_json<T: serde::de::DeserializeOwned>(resp: Response) -> Result<T> {
    match resp {
        Response::Json { value } => Ok(serde_json::from_value(value)?),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

fn lookup_or_pull(cli: &Cli, name: &str) -> Result<ImageMeta> {
    match call(cli, Request::ImageShow { name: name.to_string() }) {
        Err(Error::NotFound(_)) if collocate_registry::Reference::parse(name).is_ok() => {
            narrate(cli, &format!("Pulling {name}..."));
            let mut c = crate::transport::api(cli)?;
            collocate_compose::up::pull_image(c.as_mut(), name, PullPolicy::Missing, Vec::new())
        }
        other => image_json(other?),
    }
}

fn import_image(cli: &Cli, file: &Path) -> Result<Vec<ImageMeta>> {
    let mut reader = crate::transport::ArchiveReader::open(file)?;
    if let Some(resp) = crate::transport::with_remote_api(cli, |api| {
        let length = reader.length();
        api.import(&mut reader, length)
    })? {
        return image_json(resp);
    }
    let mut c = crate::transport::local(cli)?;
    c.send_with_fds(&Request::ImageImport, &[&reader.as_raw_fd()])?;
    image_json(c.read_response()?)
}

fn image(cli: &Cli, cmd: &ImageCmd) -> Result<i32> {
    match cmd {
        ImageCmd::Import { file } => {
            let imported = import_image(cli, file)?;
            if cli.format == Format::Json {
                json(&imported);
            } else {
                for m in &imported {
                    narrate(cli, &success_line("Imported", &m.name));
                }
            }
            for m in &imported {
                if !m.config.volumes.is_empty() {
                    eprintln!("note: {} declares volumes {:?}; mount host paths there with -v to keep the data", m.name, m.config.volumes);
                }
            }
            Ok(0)
        }
        ImageCmd::Pull { reference, username, password_stdin } => {
            let parsed = collocate_registry::Reference::parse(reference).map_err(|e| Error::Invalid(e.to_string()))?;
            let password = if *password_stdin { Some(read_stdin()?) } else { None };
            let credentials = match username {
                Some(u) => vec![collocate_core::request::RegistryCredential {
                    registry: parsed.registry.clone(),
                    username: u.clone(),
                    password: password.unwrap_or_default(),
                }],
                None => Vec::new(),
            };
            let mut c = crate::transport::api(cli)?;
            let meta = collocate_compose::up::pull_image(c.as_mut(), reference, PullPolicy::Always, credentials)?;
            if cli.format == Format::Json {
                json(&meta);
            } else {
                narrate(cli, &success_line("Pulled", &meta.name));
            }
            Ok(0)
        }
        ImageCmd::List => {
            let list: Vec<ImageMeta> = image_json(call(cli, Request::ImageList)?)?;
            if cli.format == Format::Json {
                json(&list);
            } else if list.is_empty() {
                print!("{}", empty_state("No images found."));
            } else {
                let rows: Vec<Vec<String>> = list
                    .iter()
                    .map(|m| vec![m.name.clone(), m.kind.label().to_string(), m.layers.len().to_string(), m.digest.clone()])
                    .collect();
                print!("{}", render_with(&["IMAGE", "KIND", "LAYERS", "DIGEST"], &rows, &table_opts(&[], false, false)));
            }
            Ok(0)
        }
        ImageCmd::Show { name } => {
            let m: ImageMeta = image_json(call(cli, Request::ImageShow { name: name.clone() })?)?;
            if cli.format == Format::Json {
                json(&m);
            } else {
                println!("{}", describe_image(&m));
            }
            Ok(0)
        }
        ImageCmd::Delete { name } => {
            if !confirm_action(cli, &format!("Delete image {name}?"))? {
                return Ok(abort());
            }
            call(cli, Request::ImageDelete { name: name.clone() })?;
            narrate(cli, &success_line("Deleted", name));
            Ok(0)
        }
        ImageCmd::Prune => {
            if !confirm_action(cli, "Prune unreferenced image layers?")? {
                return Ok(abort());
            }
            let removed: Vec<String> = image_json(call(cli, Request::ImagePrune)?)?;
            if cli.format == Format::Json {
                json(&removed);
            } else if removed.is_empty() {
                narrate_detail(cli, "No unreferenced layers to remove.");
            } else {
                for l in &removed {
                    narrate(cli, &success_line("Removed layer", l));
                }
            }
            Ok(0)
        }
    }
}

fn node(cli: &Cli, cmd: &NodeCmd) -> Result<i32> {
    if Client::connect(&cli.host).is_ok() {
        return Err(Error::Invalid(format!("{} is reachable; stop collocated before running this", cli.host.display())));
    }
    let run_dir = cli.host.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/run/collocate"));
    let store = collocate_store::NodeStore::open(cli.state_dir.clone(), run_dir)?;
    let id = collocate_store::instance_id();
    let (verb, meta) = match cmd {
        NodeCmd::Reinit => ("Reinitialized", store.reinit_node(&id)?),
        NodeCmd::Adopt => ("Adopted", store.adopt_node(&id)?),
    };
    if cli.format == Format::Json {
        json(&meta);
    } else {
        println!("node: {}\nuuid: {}", meta.name, meta.node_uuid);
    }
    narrate(cli, &success_line(verb, &meta.name));
    Ok(0)
}

struct ClusterCtx {
    lxc: Lxc,
    target: LxdTarget,
    relay: String,
    registry: Registry,
    registered: bool,
}

fn cluster_ctx(lxc: &str, relay: Option<&str>) -> Result<ClusterCtx> {
    let layout = Layout::detect();
    let loaded = Registry::load(&layout.cluster_registry)?;
    let registered = loaded.is_some();
    let registry = loaded.unwrap_or_default();
    let relay = relay.map(String::from).unwrap_or_else(|| if registered { registry.relay() } else { layout.relay.clone() });
    Ok(ClusterCtx { lxc: Lxc::new(lxc), target: registry.target(), relay, registry, registered })
}

fn save_registry(ctx: &ClusterCtx) -> Result<()> {
    ctx.registry.save(&Layout::detect().cluster_registry)
}

fn node_apis(ctx: &ClusterCtx, nodes: &[String]) -> Result<Vec<(String, Box<dyn collocate_core::client::Api>)>> {
    nodes
        .iter()
        .map(|n| {
            Ok((n.clone(), Box::new(crate::init::node_api(&ctx.lxc, &ctx.target, n, &ctx.relay)?) as Box<dyn collocate_core::client::Api>))
        })
        .collect()
}

fn record_of(def: &collocate_compose::model::NodeDef) -> NodeRecord {
    NodeRecord { target: def.target.clone(), image: def.image.clone(), cpus: def.cpus, memory: def.memory.clone() }
}

fn cluster_status_rows(statuses: &[collocate_cluster::status::NodeStatus]) -> Vec<Vec<String>> {
    statuses
        .iter()
        .flat_map(|s| {
            if s.containers.is_empty() {
                vec![vec![
                    s.name.clone(),
                    if s.reachable { "reachable".into() } else { format!("unreachable: {}", s.error.clone().unwrap_or_default()) },
                    "—".into(),
                    "—".into(),
                ]]
            } else {
                s.containers.iter().map(|c| vec![s.name.clone(), "reachable".into(), c.name.clone(), state_word(c.state).into()]).collect()
            }
        })
        .collect()
}

fn cluster(cli: &Cli, cmd: &ClusterCmd) -> Result<i32> {
    match cmd {
        ClusterCmd::List { lxc } => {
            let ctx = cluster_ctx(lxc, None)?;
            let states =
                collocate_cluster::provision::instance_states(&ctx.lxc.run(&collocate_cluster::provision::list_args(&ctx.target), None)?)?;
            let rows: Vec<Vec<String>> = if ctx.registered {
                ctx.registry
                    .nodes
                    .iter()
                    .map(|(n, rec)| {
                        vec![
                            n.clone(),
                            states.get(n).cloned().unwrap_or_else(|| "Missing".into()),
                            rec.target.clone().unwrap_or_else(|| "—".into()),
                        ]
                    })
                    .collect()
            } else {
                states.iter().filter(|(_, st)| st.as_str() == "Running").map(|(n, st)| vec![n.clone(), st.clone(), "—".into()]).collect()
            };
            if cli.format == Format::Json {
                let objs: Vec<serde_json::Value> =
                    rows.iter().map(|r| serde_json::json!({"node": r[0], "state": r[1], "target": r[2]})).collect();
                json(&objs);
            } else if rows.is_empty() {
                print!("{}", empty_state("No nodes found."));
            } else {
                print!("{}", render_with(&["NODE", "STATE", "TARGET"], &rows, &table_opts(&[], false, false)));
            }
            Ok(0)
        }
        ClusterCmd::AddNode(a) => {
            let mut ctx = cluster_ctx(&a.lxc, None)?;
            if !collocate_cluster::preseed::valid_instance_name(&a.name) {
                return Err(Error::Invalid(format!("node name {:?} is not a valid LXD instance name", a.name)));
            }
            if let Some(i) = &a.install {
                ctx.registry.install = i.clone();
            }
            let install = Install::parse(&ctx.registry.install)?;
            let rec = NodeRecord { target: a.target.clone(), image: a.image.clone(), cpus: a.cpus, memory: a.memory.clone() };
            let nodes: std::collections::BTreeMap<String, NodeRecord> = [(a.name.clone(), rec.clone())].into();
            crate::init::provision_nodes(cli, &ctx.lxc, &ctx.target, &nodes, &ctx.registry.image, &install, &ctx.registry.daemon)?;
            ctx.registry.nodes.insert(a.name.clone(), rec);
            save_registry(&ctx)?;
            narrate(cli, &success_line("Added node", &a.name));
            Ok(0)
        }
        ClusterCmd::RemoveNode { name, keep_instance, lxc } => {
            let mut ctx = cluster_ctx(lxc, None)?;
            if !ctx.registry.nodes.contains_key(name) {
                return Err(Error::NotFound(format!("node {name} is not in the cluster registry")));
            }
            let prompt = if *keep_instance {
                format!("Remove node {name} from the cluster registry?")
            } else {
                format!("Delete node {name} and every container on it?")
            };
            if !confirm_action(cli, &prompt)? {
                return Ok(abort());
            }
            if !keep_instance {
                ctx.lxc.run(&collocate_cluster::provision::delete_args(&ctx.target, name), None)?;
            }
            ctx.registry.nodes.remove(name);
            save_registry(&ctx)?;
            narrate(cli, &success_line("Removed node", name));
            Ok(0)
        }
        ClusterCmd::Status(a) => {
            let ctx = cluster_ctx(&a.lxc, a.relay.as_deref())?;
            let (nodes, project): (Vec<String>, Option<String>) = if a.file.is_file() {
                let file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
                let nodes =
                    if file.nodes.is_empty() { ctx.registry.nodes.keys().cloned().collect() } else { file.nodes.keys().cloned().collect() };
                (nodes, Some(file.project))
            } else {
                (ctx.registry.nodes.keys().cloned().collect(), None)
            };
            let mut apis = node_apis(&ctx, &nodes)?;
            let statuses = collocate_cluster::status::cluster_status(&mut apis, project.as_deref());
            if cli.format == Format::Json {
                let objs: Vec<serde_json::Value> = statuses
                    .iter()
                    .map(|s| serde_json::json!({"node": s.name, "reachable": s.reachable, "containers": s.containers.len(), "error": s.error}))
                    .collect();
                json(&objs);
            } else if statuses.is_empty() {
                print!("{}", empty_state("No nodes found."));
            } else {
                print!(
                    "{}",
                    render_with(&["NODE", "STATUS", "CONTAINER", "STATE"], &cluster_status_rows(&statuses), &table_opts(&[], false, false))
                );
            }
            Ok(i32::from(statuses.iter().any(|s| !s.reachable)))
        }
        ClusterCmd::Up(a) => {
            let mut file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
            let mut ctx = cluster_ctx(&a.lxc, a.relay.as_deref())?;
            if file.nodes.is_empty() {
                if ctx.registry.nodes.is_empty() {
                    return Err(Error::Invalid(
                        "the compose file declares no nodes and the cluster registry is empty; run 'collocate init --mode lxd' first"
                            .into(),
                    ));
                }
                for n in ctx.registry.nodes.keys() {
                    file.nodes.insert(n.clone(), collocate_compose::model::NodeDef::default());
                }
            }
            for (name, svc) in &file.services {
                if let Some(n) = &svc.node {
                    if !file.nodes.contains_key(n) {
                        return Err(Error::Invalid(format!(
                            "service {name} uses node {n}, which is neither declared in the file nor registered"
                        )));
                    }
                }
            }
            let cross = collocate_cluster::plan::cross_node_references(&file);
            if !cross.is_empty() {
                return Err(Error::Invalid(format!(
                    "cross-node address references need a routable cluster network and are not supported yet: {}",
                    cross.join("; ")
                )));
            }
            if let Some(i) = &a.install {
                ctx.registry.install = i.clone();
            }
            let install = Install::parse(&ctx.registry.install)?;
            let mut records = std::collections::BTreeMap::new();
            for (name, def) in &file.nodes {
                let mut rec = record_of(def);
                if let Some(known) = ctx.registry.nodes.get(name) {
                    rec.target = rec.target.or_else(|| known.target.clone());
                    rec.image = rec.image.or_else(|| known.image.clone());
                    rec.cpus = rec.cpus.or(known.cpus);
                    rec.memory = rec.memory.or_else(|| known.memory.clone());
                }
                records.insert(name.clone(), rec);
            }
            crate::init::provision_nodes(cli, &ctx.lxc, &ctx.target, &records, &ctx.registry.image, &install, &ctx.registry.daemon)?;
            ctx.relay = install.relay().to_string();
            ctx.registry.nodes.extend(records);
            save_registry(&ctx)?;
            let mut subs: Vec<(String, ComposeFile)> = Vec::new();
            for node in file.nodes.keys() {
                let mut sub = file.clone();
                sub.nodes.clear();
                sub.services.retain(|name, _| collocate_cluster::plan::node_of(&file, name).as_deref() == Some(node.as_str()));
                for s in sub.services.values_mut() {
                    s.node = None;
                    s.depends_on.retain(|d| {
                        file.services
                            .get(&d.service)
                            .is_some_and(|_| collocate_cluster::plan::node_of(&file, &d.service).as_deref() == Some(node.as_str()))
                    });
                }
                sub.loadbalancers.retain(|_, lb| sub.services.contains_key(&lb.backends.service));
                if !sub.services.is_empty() {
                    subs.push((node.clone(), sub));
                }
            }
            let mut apis: std::collections::BTreeMap<String, Box<dyn collocate_core::client::Api>> =
                node_apis(&ctx, &subs.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>())?.into_iter().collect();
            collocate_cluster::secrets::replicate_secrets(&file, &mut apis)?;
            for (node, sub) in &subs {
                let api = apis.get_mut(node).ok_or_else(|| Error::Internal(format!("no connection to node {node}")))?;
                let subnet = match api.call(Request::Info)? {
                    Response::Text { text } => serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| v["subnet"].as_str().map(String::from))
                        .unwrap_or_else(|| collocate_core::settings::DEFAULT_SUBNET.into()),
                    _ => collocate_core::settings::DEFAULT_SUBNET.into(),
                };
                let opts = UpOptions {
                    subnet,
                    base_dir: a
                        .file
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf),
                    regenerate_secrets: None,
                    dry_run: false,
                    ready_timeout: Duration::from_secs(a.timeout),
                    poll_interval: Duration::from_millis(250),
                };
                let report = up(api.as_mut(), sub, &opts)?;
                for (label, names) in [
                    ("Created", &report.created),
                    ("Recreated", &report.recreated),
                    ("Started", &report.started),
                    ("Unchanged", &report.kept),
                ] {
                    for n in names {
                        narrate(cli, &format!("{node}: {}", success_line(label, n)));
                    }
                }
            }
            Ok(0)
        }
    }
}
