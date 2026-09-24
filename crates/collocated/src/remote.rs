use crate::config::Config;
use collocate_core::auth::{Access, Caller, Role, Scope};
use collocate_core::request::{Request, Response};
use collocate_core::settings::parse_listen_address;
use collocate_core::{Error, Result};
use collocate_trust::{now_secs, Identity, Token, TrustStore};
use std::net::IpAddr;
use std::process::Command;

pub fn trust_store(cfg: &Config) -> Result<TrustStore> {
    TrustStore::open(cfg.state_dir.join("trust"))
}

pub fn host_addresses(ip_json: &str, exclude: &[&str]) -> Vec<IpAddr> {
    let links: Vec<serde_json::Value> = serde_json::from_str(ip_json).unwrap_or_default();
    let mut out = Vec::new();
    for link in &links {
        let name = link["ifname"].as_str().unwrap_or("");
        if name == "lo" || exclude.contains(&name) {
            continue;
        }
        for info in link["addr_info"].as_array().into_iter().flatten() {
            if info["scope"].as_str() != Some("global") {
                continue;
            }
            if let Some(ip) = info["local"].as_str().and_then(|a| a.parse::<IpAddr>().ok()) {
                if !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
    }
    out
}

fn format_address(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(v4) => format!("{v4}:{port}"),
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
    }
}

pub fn token_addresses(https_address: &str, host: &[IpAddr]) -> Result<Vec<String>> {
    let (ip, port) = parse_listen_address(https_address)?;
    let addrs: Vec<String> = match ip {
        Some(ip) if !ip.is_unspecified() => vec![format_address(ip, port)],
        Some(IpAddr::V4(_)) => host.iter().filter(|a| a.is_ipv4()).map(|a| format_address(*a, port)).collect(),
        _ => host.iter().map(|a| format_address(*a, port)).collect(),
    };
    if addrs.is_empty() {
        return Err(Error::Invalid(format!("no address of this host matches the https address {https_address}")));
    }
    Ok(addrs)
}

fn local_addresses(cfg: &Config) -> Vec<IpAddr> {
    Command::new("ip")
        .args(["-j", "addr", "show"])
        .output()
        .map(|o| host_addresses(&String::from_utf8_lossy(&o.stdout), &[cfg.bridge.as_str()]))
        .unwrap_or_default()
}

pub fn ensure_identity(cfg: &Config) -> Result<Identity> {
    let mut names: Vec<String> = local_addresses(cfg).iter().map(ToString::to_string).collect();
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        names.push(h.trim().to_string());
    }
    trust_store(cfg)?.ensure_server_identity(&names)
}

pub fn server_fingerprint(cfg: &Config) -> Option<String> {
    trust_store(cfg).ok()?.server_identity().ok()?.map(|i| i.fingerprint)
}

fn json<T: serde::Serialize>(v: &T) -> Result<Response> {
    Ok(Response::Json { value: serde_json::to_value(v)? })
}

pub fn handle_trust(cfg: &Config, req: Request) -> Result<Response> {
    let store = trust_store(cfg)?;
    let now = now_secs();
    match req {
        Request::TrustTokenCreate { name, role, projects, expiry_secs } => {
            let https = cfg.https_address.clone().ok_or_else(|| {
                Error::Invalid("remote access is disabled; enable it first with 'sudo collocate init --https-address :8443'".into())
            })?;
            let identity = ensure_identity(cfg)?;
            let addresses = token_addresses(&https, &local_addresses(cfg))?;
            let (secret, pending) = store.create_token(&name, role, &projects, expiry_secs, now)?;
            let token = Token {
                client_name: name.clone(),
                fingerprint: identity.fingerprint,
                addresses,
                secret,
                expires_at: pending.expires_at,
                role,
                projects: pending.projects.clone(),
            };
            json(
                &serde_json::json!({"name": name, "token": token.encode()?, "expires_at": pending.expires_at, "role": role, "projects": pending.projects}),
            )
        }
        Request::TrustTokenList => {
            let list: Vec<serde_json::Value> = store
                .tokens()?
                .into_iter()
                .map(|t| {
                    serde_json::json!({"name": t.name, "role": t.role, "projects": t.projects, "created_at": t.created_at, "expires_at": t.expires_at, "expired": t.expired(now)})
                })
                .collect();
            json(&list)
        }
        Request::TrustTokenRevoke { name } => {
            store.revoke_token(&name)?;
            Ok(Response::Ok)
        }
        Request::TrustList => json(&store.certificates()?),
        Request::TrustRemove { name } => json(&store.remove(&name)?),
        Request::TrustAddCertificate { name, certificate, role, projects } => {
            json(&store.add_certificate(&name, &certificate, role, &projects, now)?)
        }
        Request::TrustEnroll { secret, certificate, name } => json(&store.enroll(&secret, &certificate, name.as_deref(), now)?),
        Request::TrustLookup { fingerprint } => {
            let cert = store.lookup(&fingerprint)?.ok_or_else(|| Error::NotFound("certificate is not trusted".into()))?;
            json(&cert.caller())
        }
        other => Err(Error::Internal(format!("not a trust request: {}", other.verb()))),
    }
}

fn restricted_message(caller: &Caller) -> String {
    format!("{} is restricted to projects {}", caller.short(), caller.projects.join(", "))
}

pub fn authorize(
    caller: &Caller,
    verb: &str,
    access: &Access,
    target_project: &mut dyn FnMut(&str) -> Result<Option<String>>,
) -> Result<bool> {
    if access.scope == Scope::Internal {
        return Err(Error::Forbidden(format!("{verb} cannot be called remotely")));
    }
    if caller.role < access.role {
        return Err(Error::Forbidden(format!(
            "{} has the {} role; {verb} needs {}",
            caller.short(),
            caller.role.label(),
            access.role.label()
        )));
    }
    match &access.scope {
        Scope::Open | Scope::Internal => Ok(false),
        Scope::Project(p) => {
            if caller.allows_project(p.as_deref()) {
                Ok(false)
            } else {
                Err(Error::Forbidden(restricted_message(caller)))
            }
        }
        Scope::Filtered(p) => match p {
            Some(_) if !caller.allows_project(p.as_deref()) => Err(Error::Forbidden(restricted_message(caller))),
            Some(_) => Ok(false),
            None => Ok(caller.restricted()),
        },
        Scope::Target(t) => {
            if caller.restricted() && !caller.allows_project(target_project(t)?.as_deref()) {
                Err(Error::Forbidden(restricted_message(caller)))
            } else {
                Ok(false)
            }
        }
        Scope::Unrestricted => {
            if caller.restricted() {
                Err(Error::Forbidden(format!("{}; {verb} needs an unrestricted certificate", restricted_message(caller))))
            } else {
                Ok(false)
            }
        }
    }
}

pub fn filter_response(resp: Response, caller: &Caller) -> Response {
    let ok = |p: &Option<String>| caller.allows_project(p.as_deref());
    match resp {
        Response::Containers(c) => Response::Containers(c.into_iter().filter(|x| ok(&x.project)).collect()),
        Response::Stats(s) => Response::Stats(s.into_iter().filter(|x| ok(&x.project)).collect()),
        Response::Lbs(l) => Response::Lbs(l.into_iter().filter(|x| ok(&Some(x.spec.project.clone()))).collect()),
        Response::Names(n) => {
            Response::Names(n.into_iter().filter(|x| x.split_once('/').is_some_and(|(p, _)| caller.allows_project(Some(p)))).collect())
        }
        other => other,
    }
}

pub fn audited(access: &Access) -> bool {
    access.role >= Role::Operator
}
