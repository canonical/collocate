use crate::cli::{Cli, RemoteCmd, TrustCmd, TrustTokenCmd};
use crate::commands::{abort, confirm_action, json, narrate};
use crate::output::{is_interactive, success_line, terminal_width, Format};
use crate::table::{empty_state, render_with, TableOptions};
use collocate_core::auth::{valid_projects, Role};
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use collocate_remote::client::{enroll, Endpoint, HttpsApi};
use collocate_remote::config::{client_identity, config_dir, valid_remote_name, Remote, RemoteConfig, LOCAL};
use collocate_trust::Token;

pub fn parse_duration(s: &str) -> Result<Option<u64>> {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() || t == "0" || t == "never" || t == "none" {
        return Ok(None);
    }
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = num.parse().map_err(|_| Error::Invalid(format!("invalid duration {s:?}; use forms like 90s, 30m, 24h or 7d")))?;
    let factor = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        _ => return Err(Error::Invalid(format!("invalid duration unit in {s:?}; use s, m, h, d or w"))),
    };
    Ok(Some(n.saturating_mul(factor)).filter(|v| *v > 0))
}

fn table(headers: &[&str], rows: &[Vec<String>]) {
    let opts = TableOptions {
        columns: vec![],
        no_headers: false,
        no_truncate: false,
        interactive: is_interactive(),
        max_width: terminal_width(80),
    };
    print!("{}", render_with(headers, rows, &opts));
}

fn projects_label(v: &serde_json::Value) -> String {
    let list: Vec<String> = v.as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(String::from)).collect()).unwrap_or_default();
    if list.is_empty() {
        "all".into()
    } else {
        list.join(",")
    }
}

fn when(ts: Option<u64>) -> String {
    match ts {
        None => "never".into(),
        Some(t) => {
            let now = collocate_trust::now_secs();
            if t <= now {
                "expired".into()
            } else {
                let left = t - now;
                match left {
                    0..=3599 => format!("in {}m", left / 60 + 1),
                    3600..=172799 => format!("in {}h", left / 3600),
                    _ => format!("in {}d", left / 86400),
                }
            }
        }
    }
}

fn json_of(resp: Response) -> Result<serde_json::Value> {
    match resp {
        Response::Json { value } => Ok(value),
        other => Err(Error::Internal(format!("unexpected response {other:?}"))),
    }
}

pub fn trust(cli: &Cli, cmd: &TrustCmd) -> Result<i32> {
    let call = |req: Request| crate::transport::call(cli, req);
    match cmd {
        TrustCmd::Add { name, role, projects, expiry } => {
            valid_projects(projects)?;
            let v = json_of(call(Request::TrustTokenCreate {
                name: name.clone(),
                role: Role::parse(role)?,
                projects: projects.clone(),
                expiry_secs: expiry.as_deref().map(parse_duration).transpose()?.flatten(),
            })?)?;
            if cli.format == Format::Json {
                json(&v);
            } else {
                println!("{}", v["token"].as_str().unwrap_or_default());
            }
            narrate(
                cli,
                &format!(
                    "Created a trust token for {name} ({} role, projects: {}, expires: {}). Use it with: collocate remote add NAME TOKEN",
                    v["role"].as_str().unwrap_or(""),
                    projects_label(&v["projects"]),
                    when(v["expires_at"].as_u64())
                ),
            );
            Ok(0)
        }
        TrustCmd::List => {
            let v = json_of(call(Request::TrustList)?)?;
            if cli.format == Format::Json {
                let mut list = v.clone();
                for c in list.as_array_mut().into_iter().flatten() {
                    if let Some(o) = c.as_object_mut() {
                        o.remove("certificate");
                    }
                }
                json(&list);
                return Ok(0);
            }
            let rows: Vec<Vec<String>> = v
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    vec![
                        c["name"].as_str().unwrap_or("").to_string(),
                        c["role"].as_str().unwrap_or("").to_string(),
                        projects_label(&c["projects"]),
                        c["fingerprint"].as_str().unwrap_or("").chars().take(12).collect(),
                    ]
                })
                .collect();
            if rows.is_empty() {
                print!("{}", empty_state("No trusted clients found."));
            } else {
                table(&["NAME", "ROLE", "PROJECTS", "FINGERPRINT"], &rows);
            }
            Ok(0)
        }
        TrustCmd::Show { name } => {
            let v = json_of(call(Request::TrustList)?)?;
            let found = v
                .as_array()
                .into_iter()
                .flatten()
                .find(|c| {
                    c["name"].as_str() == Some(name.as_str())
                        || c["fingerprint"].as_str().is_some_and(|f| name.len() >= 12 && f.starts_with(name.as_str()))
                })
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("trusted client {name}")))?;
            if cli.format == Format::Json {
                json(&found);
            } else {
                println!(
                    "name: {}\nrole: {}\nprojects: {}\nfingerprint: {}\n{}",
                    found["name"].as_str().unwrap_or(""),
                    found["role"].as_str().unwrap_or(""),
                    projects_label(&found["projects"]),
                    found["fingerprint"].as_str().unwrap_or(""),
                    found["certificate"].as_str().unwrap_or("").trim_end()
                );
            }
            Ok(0)
        }
        TrustCmd::Remove { name } => {
            if !confirm_action(cli, &format!("Stop trusting {name}? Its open sessions are cut off."))? {
                return Ok(abort());
            }
            let v = json_of(call(Request::TrustRemove { name: name.clone() })?)?;
            narrate(cli, &success_line("Removed trust for", v["name"].as_str().unwrap_or(name)));
            Ok(0)
        }
        TrustCmd::AddCertificate { name, file, role, projects } => {
            let pem = std::fs::read_to_string(file).map_err(|e| Error::Invalid(format!("{}: {e}", file.display())))?;
            let v = json_of(call(Request::TrustAddCertificate {
                name: name.clone(),
                certificate: pem,
                role: Role::parse(role)?,
                projects: projects.clone(),
            })?)?;
            if cli.format == Format::Json {
                json(&v);
            }
            narrate(
                cli,
                &success_line(
                    "Trusted",
                    &format!("{name} ({})", v["fingerprint"].as_str().unwrap_or("").chars().take(12).collect::<String>()),
                ),
            );
            Ok(0)
        }
        TrustCmd::Token(TrustTokenCmd::List) => {
            let v = json_of(call(Request::TrustTokenList)?)?;
            if cli.format == Format::Json {
                json(&v);
                return Ok(0);
            }
            let rows: Vec<Vec<String>> = v
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| {
                    vec![
                        t["name"].as_str().unwrap_or("").to_string(),
                        t["role"].as_str().unwrap_or("").to_string(),
                        projects_label(&t["projects"]),
                        when(t["expires_at"].as_u64()),
                    ]
                })
                .collect();
            if rows.is_empty() {
                print!("{}", empty_state("No pending trust tokens found."));
            } else {
                table(&["NAME", "ROLE", "PROJECTS", "EXPIRES"], &rows);
            }
            Ok(0)
        }
        TrustCmd::Token(TrustTokenCmd::Revoke { name }) => {
            call(Request::TrustTokenRevoke { name: name.clone() })?;
            narrate(cli, &success_line("Revoked the trust token", name));
            Ok(0)
        }
    }
}

pub fn remote(cli: &Cli, cmd: &RemoteCmd) -> Result<i32> {
    let dir = config_dir();
    let mut cfg = RemoteConfig::load(&dir)?;
    match cmd {
        RemoteCmd::Add { name, token, client_name } => {
            valid_remote_name(name)?;
            if cfg.remotes.contains_key(name) {
                return Err(Error::Conflict(format!("remote {name} already exists")));
            }
            let token = Token::decode(token)?;
            let identity = client_identity(&dir)?;
            let enrolled = enroll(&token, &identity, client_name.as_deref())?;
            let remote = Remote { addresses: token.addresses.clone(), fingerprint: token.fingerprint.clone() };
            let mut api =
                HttpsApi::new(Endpoint { addresses: remote.addresses.clone(), fingerprint: remote.fingerprint.clone() }, identity);
            let info = api.server_info()?;
            if info["auth"].as_str() != Some("trusted") {
                return Err(Error::Internal("the server accepted the token but does not trust this client".into()));
            }
            cfg.remotes.insert(name.clone(), remote);
            cfg.save(&dir)?;
            if cli.format == Format::Json {
                json(&enrolled);
            }
            narrate(
                cli,
                &format!(
                    "Added remote {name} as {} ({} role, projects: {}).",
                    enrolled["name"].as_str().unwrap_or(""),
                    enrolled["role"].as_str().unwrap_or(""),
                    projects_label(&enrolled["projects"])
                ),
            );
            Ok(0)
        }
        RemoteCmd::List => {
            let default = cfg.default_remote().to_string();
            let mut rows = vec![vec![
                if default == LOCAL { format!("{LOCAL} (default)") } else { LOCAL.to_string() },
                cli.host.display().to_string(),
                "—".into(),
            ]];
            for (name, r) in &cfg.remotes {
                rows.push(vec![
                    if *name == default { format!("{name} (default)") } else { name.clone() },
                    r.addresses.join(","),
                    r.fingerprint.chars().take(12).collect(),
                ]);
            }
            if cli.format == Format::Json {
                json(&serde_json::json!({"default": default, "remotes": cfg.remotes}));
            } else {
                table(&["NAME", "ADDRESS", "FINGERPRINT"], &rows);
            }
            Ok(0)
        }
        RemoteCmd::Remove { name } => {
            if cfg.remotes.remove(name).is_none() {
                return Err(Error::NotFound(format!("remote {name}")));
            }
            if cfg.default.as_deref() == Some(name.as_str()) {
                cfg.default = None;
            }
            cfg.save(&dir)?;
            narrate(cli, &success_line("Removed remote", name));
            Ok(0)
        }
        RemoteCmd::Rename { old, new } => {
            valid_remote_name(new)?;
            if cfg.remotes.contains_key(new) {
                return Err(Error::Conflict(format!("remote {new} already exists")));
            }
            let r = cfg.remotes.remove(old).ok_or_else(|| Error::NotFound(format!("remote {old}")))?;
            cfg.remotes.insert(new.clone(), r);
            if cfg.default.as_deref() == Some(old.as_str()) {
                cfg.default = Some(new.clone());
            }
            cfg.save(&dir)?;
            narrate(cli, &success_line("Renamed remote", &format!("{old} to {new}")));
            Ok(0)
        }
        RemoteCmd::Switch { name } => {
            if name == LOCAL {
                cfg.default = None;
            } else {
                cfg.get(name)?;
                cfg.default = Some(name.clone());
            }
            cfg.save(&dir)?;
            narrate(cli, &format!("Default remote is now {name}."));
            Ok(0)
        }
        RemoteCmd::GetDefault => {
            println!("{}", cfg.default_remote());
            Ok(0)
        }
    }
}
