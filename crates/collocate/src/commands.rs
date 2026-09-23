use crate::cli::{Cli, ClusterArgs, ClusterCmd, Command, ComposeArgs, ImageCmd, LoadBalancerArgs, LoadBalancerCmd, NodeCmd, SecretCmd};
use crate::output::{self, confirm, is_interactive, success_line, terminal_width, Format, Verbosity};
use crate::runspec::build_spec;
use crate::status::{format_ports, format_process, format_uptime, read_listening, read_procs, read_uptime};
use crate::table::{empty_state, render_with, TableOptions};
use collocate_compose::build::signal_number;
use collocate_compose::convert::convert_docker_compose;
use collocate_compose::model::ComposeFile;
use collocate_compose::up::{down, plan_only, up, UpOptions};
use collocate_core::client::Client;
use collocate_core::net::{Algorithm, NoBackends, Proto};
use collocate_core::request::{ContainerInfo, LbSpec, LbStatus, Request, Response, State};
use collocate_core::spec::Series;
use collocate_core::{Error, Result};
use collocate_image::config::ImageMeta;
use collocate_sys::fdpass::SendWithFds;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn connect(cli: &Cli) -> Result<Client<UnixStream>> {
    Client::connect(&cli.host)
}

fn call(cli: &Cli, req: Request) -> Result<Response> {
    connect(cli)?.call(&req)
}

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

fn json<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn narrate(cli: &Cli, message: &str) {
    output::narrate(cli.verbosity(), Verbosity::Brief, message);
}

fn narrate_detail(cli: &Cli, message: &str) {
    output::narrate(cli.verbosity(), Verbosity::Verbose, message);
}

fn abort() -> i32 {
    eprintln!("Aborted.");
    1
}

fn confirm_action(cli: &Cli, prompt: &str) -> Result<bool> {
    confirm(prompt, false, cli.yes)
}

fn table_opts(columns: &[String], no_headers: bool, no_truncate: bool) -> TableOptions {
    TableOptions { columns: columns.to_vec(), no_headers, no_truncate, interactive: is_interactive(), max_width: terminal_width(80) }
}

fn follow_logs(cli: &Cli, target: &str, until_exit: bool, tail: Option<usize>) -> Result<()> {
    let mut offset = match call(cli, Request::Logs { target: target.into(), tail, offset: None })? {
        Response::Log { data, next_offset } => {
            print!("{data}");
            next_offset
        }
        _ => 0,
    };
    loop {
        let running = containers(cli, true, None)?
            .iter()
            .any(|c| (c.name == target || c.id.to_string().starts_with(target)) && c.state == State::Running);
        if let Response::Log { data, next_offset } = call(cli, Request::Logs { target: target.into(), tail: None, offset: Some(offset) })? {
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

fn read_stdin() -> Result<String> {
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
            c.series.clone().unwrap_or_else(|| "—".into()),
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
        "name: {}\ndigest: {}\nlayers: {}\nentrypoint: {:?}\ncmd: {:?}\nuser: {}\nworkdir: {}",
        m.name,
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
            let store = collocate_image::config::ImageStore::new(&cli.state_dir);
            let spec = build_spec(a, host_series(), &|k| std::env::var(k).ok(), &|p| Ok(std::fs::read_to_string(p)?), &|n| store.get(n))?;
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
            follow_logs(cli, &target, true, None)?;
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
        Command::Logs { target, follow, tail } => {
            follow_logs(cli, target, *follow, *tail)?;
            Ok(0)
        }
        Command::Exec { env, user, workdir, timeout, target, command } => {
            let envs = env.iter().filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
            let mut c = connect(cli)?;
            let (i, o, e) = (std::io::stdin(), std::io::stdout(), std::io::stderr());
            let req = Request::Exec {
                target: target.clone(),
                argv: command.clone(),
                env: envs,
                user: user.clone(),
                workdir: workdir.clone(),
                tty: false,
                timeout_secs: *timeout,
            };
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
            let mut c = connect(cli)?;
            let report = up(&mut c, &file, &compose_options(cli, a, &a.file)?)?;
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
            let mut c = connect(cli)?;
            let p = plan_only(&mut c, &file, &compose_options(cli, a, &a.file)?)?;
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
            let mut c = connect(cli)?;
            let removed = down(&mut c, &file, &compose_options(cli, a, &a.file)?)?;
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
            let daemon_info = call(cli, Request::Info);
            if daemon_info.is_err() {
                failed = true;
            }
            if cli.format == Format::Json {
                let checks: Vec<serde_json::Value> =
                    local.checks.iter().map(|c| serde_json::json!({"name": c.name, "ok": c.ok, "detail": c.detail})).collect();
                json(&serde_json::json!({"checks": checks, "daemon_reachable": daemon_info.is_ok()}));
            } else {
                for c in &local.checks {
                    println!("{:<20} {}  {}", c.name, if c.ok { "ok " } else { "FAIL" }, c.detail);
                }
                match daemon_info {
                    Ok(Response::Text { text }) => println!("daemon: reachable ({text})"),
                    Ok(_) => {}
                    Err(e) => println!("daemon: {e}"),
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
    let mut c = connect(cli)?;
    let req = Request::Exec { target: target.to_string(), argv, env: vec![], user: None, workdir: None, tty: false, timeout_secs: None };
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
            let stdout = std::fs::File::open("/dev/null")?;
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

fn image(cli: &Cli, cmd: &ImageCmd) -> Result<i32> {
    let store = collocate_image::config::ImageStore::new(&cli.state_dir);
    match cmd {
        ImageCmd::Import { file } => {
            let imported = if file == Path::new("-") {
                collocate_image::import::import_archive(std::io::stdin(), &cli.state_dir)?
            } else {
                collocate_image::import::import_archive(std::fs::File::open(file)?, &cli.state_dir)?
            };
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
        ImageCmd::List => {
            let list = store.list()?;
            if cli.format == Format::Json {
                json(&list);
            } else if list.is_empty() {
                print!("{}", empty_state("No images found."));
            } else {
                let rows: Vec<Vec<String>> =
                    list.iter().map(|m| vec![m.name.clone(), m.layers.len().to_string(), m.digest.clone()]).collect();
                print!("{}", render_with(&["IMAGE", "LAYERS", "DIGEST"], &rows, &table_opts(&[], false, false)));
            }
            Ok(0)
        }
        ImageCmd::Show { name } => {
            let m = store.get(name)?;
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
            store.remove(name)?;
            narrate(cli, &success_line("Deleted", name));
            Ok(0)
        }
        ImageCmd::Prune => {
            if !confirm_action(cli, "Prune unreferenced image layers?")? {
                return Ok(abort());
            }
            let removed = store.gc()?;
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

fn lxc_output(program: &str, args: &[String]) -> Result<String> {
    let out = std::process::Command::new(program).args(args).output().map_err(|e| Error::Unreachable(format!("{program}: {e}")))?;
    if !out.status.success() {
        return Err(Error::Internal(format!("{program} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn node_apis(a: &ClusterArgs, file: &ComposeFile) -> Result<Vec<(String, Box<dyn collocate_core::client::Api>)>> {
    let relay = vec![a.relay.clone()];
    file.nodes
        .keys()
        .map(|n| {
            Ok((
                n.clone(),
                Box::new(collocate_cluster::transport::LxcApi::spawn(&a.lxc, n, &relay)?) as Box<dyn collocate_core::client::Api>,
            ))
        })
        .collect()
}

fn cluster(cli: &Cli, cmd: &ClusterCmd) -> Result<i32> {
    match cmd {
        ClusterCmd::List { lxc } => {
            let nodes = collocate_cluster::lxc::running_nodes(&lxc_output(lxc, &collocate_cluster::lxc::list_args())?)?;
            if cli.format == Format::Json {
                json(&nodes);
            } else if nodes.is_empty() {
                print!("{}", empty_state("No nodes found."));
            } else {
                for n in nodes {
                    println!("{n}");
                }
            }
            Ok(0)
        }
        ClusterCmd::Status(a) => {
            let file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
            let mut nodes = node_apis(a, &file)?;
            let statuses = collocate_cluster::status::cluster_status(&mut nodes, Some(&file.project));
            if cli.format == Format::Json {
                let objs: Vec<serde_json::Value> = statuses
                    .iter()
                    .map(|s| serde_json::json!({"node": s.name, "reachable": s.reachable, "containers": s.containers.len(), "error": s.error}))
                    .collect();
                json(&objs);
            } else if statuses.is_empty() {
                print!("{}", empty_state("No nodes found."));
            } else {
                let rows: Vec<Vec<String>> = statuses
                    .iter()
                    .flat_map(|s| {
                        if s.containers.is_empty() {
                            vec![vec![
                                s.name.clone(),
                                if s.reachable {
                                    "reachable".into()
                                } else {
                                    format!("unreachable: {}", s.error.clone().unwrap_or_default())
                                },
                                "—".into(),
                                "—".into(),
                            ]]
                        } else {
                            s.containers
                                .iter()
                                .map(|c| vec![s.name.clone(), "reachable".into(), c.name.clone(), state_word(c.state).into()])
                                .collect()
                        }
                    })
                    .collect();
                print!("{}", render_with(&["NODE", "STATUS", "CONTAINER", "STATE"], &rows, &table_opts(&[], false, false)));
            }
            Ok(i32::from(statuses.iter().any(|s| !s.reachable)))
        }
        ClusterCmd::Up(a) => {
            let file = ComposeFile::load(&std::fs::read_to_string(&a.file)?)?;
            let cross = collocate_cluster::plan::cross_node_references(&file);
            if !cross.is_empty() {
                return Err(Error::Invalid(format!(
                    "cross-node address references need a routable cluster network and are not supported yet: {}",
                    cross.join("; ")
                )));
            }
            let running = collocate_cluster::lxc::running_nodes(&lxc_output(&a.lxc, &collocate_cluster::lxc::list_args())?)?;
            for step in collocate_cluster::plan::provision_plan(&file, &running, &a.deb) {
                narrate_detail(cli, &format!("{step:?}"));
                lxc_output(&a.lxc, &step.lxc_args())?;
            }
            let relay = vec![a.relay.clone()];
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
            let mut node_apis: std::collections::BTreeMap<String, Box<dyn collocate_core::client::Api>> = std::collections::BTreeMap::new();
            for (node, _) in &subs {
                let api = collocate_cluster::transport::LxcApi::spawn(&a.lxc, node, &relay)?;
                node_apis.insert(node.clone(), Box::new(api));
            }
            collocate_cluster::secrets::replicate_secrets(&file, &mut node_apis)?;
            for (node, sub) in &subs {
                let api = node_apis.get_mut(node).ok_or_else(|| Error::Internal(format!("no connection to node {node}")))?;
                let subnet = match api.call(Request::Info)? {
                    Response::Text { text } => serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .and_then(|v| v["subnet"].as_str().map(String::from))
                        .unwrap_or_else(|| "172.30.0.0/16".into()),
                    _ => "172.30.0.0/16".into(),
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
