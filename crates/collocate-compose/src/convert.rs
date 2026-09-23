use crate::model::{ComposeFile, Dependency, HealthDef, Service};
use collocate_core::{Error, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub unsupported: Vec<String>,
    pub approximated: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Converted {
    pub file: ComposeFile,
    pub report: Report,
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn split_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => split_words(s),
        Value::Array(a) => a.iter().filter_map(scalar).collect(),
        _ => Vec::new(),
    }
}

fn plain_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().filter_map(scalar).collect(),
        _ => Vec::new(),
    }
}

fn convert_health(v: &Value) -> Option<HealthDef> {
    let obj = v.as_object()?;
    let test = obj.get("test")?;
    let exec = match test {
        Value::String(s) => vec!["/bin/sh".into(), "-c".into(), s.clone()],
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().filter_map(scalar).collect();
            match parts.first().map(String::as_str) {
                Some("NONE") => return None,
                Some("CMD") => parts[1..].to_vec(),
                Some("CMD-SHELL") => vec!["/bin/sh".into(), "-c".into(), parts.get(1).cloned().unwrap_or_default()],
                _ => parts,
            }
        }
        _ => return None,
    };
    let text = |k: &str| obj.get(k).and_then(scalar);
    Some(HealthDef {
        tcp: None,
        http: None,
        exec: Some(exec),
        interval: text("interval").unwrap_or_else(|| "30s".into()),
        timeout: text("timeout").unwrap_or_else(|| "30s".into()),
        retries: obj.get("retries").and_then(Value::as_u64).map_or(3, |n| n as u32),
        start_period: text("start_period").unwrap_or_else(|| "0s".into()),
    })
}

fn convert_port(v: &Value, svc: &str, report: &mut Report) -> Option<String> {
    match v {
        Value::Object(o) => {
            let target = o.get("target").and_then(scalar)?;
            let published = o.get("published").and_then(scalar)?;
            let proto = o.get("protocol").and_then(Value::as_str).unwrap_or("tcp");
            Some(if proto == "udp" { format!("{published}:{target}/udp") } else { format!("{published}:{target}") })
        }
        other => {
            let s = scalar(other)?;
            let (ports, proto) = s.split_once('/').map_or((s.as_str(), None), |(p, pr)| (p, Some(pr)));
            let parts: Vec<&str> = ports.split(':').collect();
            let pair = match parts.as_slice() {
                [h, c] => format!("{h}:{c}"),
                [ip, h, c] => {
                    report.approximated.push(format!("{svc}: port binding address {ip} ignored, published on all interfaces"));
                    format!("{h}:{c}")
                }
                _ => {
                    report.unsupported.push(format!("{svc}: port {s} has no host port"));
                    return None;
                }
            };
            Some(match proto {
                Some(p) if p != "tcp" => format!("{pair}/{p}"),
                _ => pair,
            })
        }
    }
}

fn absolutise(src: &str, base: &Path) -> String {
    if src.starts_with("./") || src.starts_with("../") || src == "." {
        let trimmed = src.strip_prefix("./").unwrap_or(src);
        let joined = if trimmed == "." { base.to_path_buf() } else { base.join(trimmed) };
        joined.to_string_lossy().to_string()
    } else {
        src.to_string()
    }
}

fn convert_volume(v: &Value, base: &Path) -> Option<String> {
    match v {
        Value::Object(o) => {
            let kind = o.get("type").and_then(Value::as_str).unwrap_or("volume");
            let src = o.get("source").and_then(scalar)?;
            let target = o.get("target").and_then(scalar)?;
            let ro = o.get("read_only").and_then(Value::as_bool).unwrap_or(false);
            let src = if kind == "bind" { absolutise(&src, base) } else { src };
            Some(if ro { format!("{src}:{target}:ro") } else { format!("{src}:{target}") })
        }
        other => {
            let s = scalar(other)?;
            let (src, rest) = s.split_once(':')?;
            Some(format!("{}:{rest}", absolutise(src, base)))
        }
    }
}

fn convert_service(name: &str, raw: &Value, base: &Path, report: &mut Report, healths: &BTreeMap<String, Option<HealthDef>>) -> Service {
    let mut svc = Service::default();
    let Some(obj) = raw.as_object() else { return svc };
    for (key, val) in obj {
        match key.as_str() {
            "image" => svc.image = scalar(val),
            "pull_policy" => match scalar(val).as_deref() {
                Some(p @ ("always" | "never" | "missing")) => svc.pull_policy = Some(p.to_string()),
                Some("if_not_present") => svc.pull_policy = Some("missing".into()),
                Some(other) => report.approximated.push(format!("{name}: pull_policy {other} mapped to missing")),
                None => {}
            },
            "command" => svc.command = string_list(val),
            "entrypoint" => svc.entrypoint = string_list(val),
            "environment" => match val {
                Value::Object(m) => {
                    for (k, v) in m {
                        match scalar(v) {
                            Some(s) => {
                                svc.env.insert(k.clone(), s);
                            }
                            None => report.unsupported.push(format!("{name}: environment {k} without a value")),
                        }
                    }
                }
                Value::Array(a) => {
                    for item in a.iter().filter_map(scalar) {
                        match item.split_once('=') {
                            Some((k, v)) => {
                                svc.env.insert(k.to_string(), v.to_string());
                            }
                            None => report.unsupported.push(format!("{name}: environment {item} without a value")),
                        }
                    }
                }
                _ => {}
            },
            "ports" => {
                if let Value::Array(a) = val {
                    svc.publish = a.iter().filter_map(|p| convert_port(p, name, report)).collect();
                }
            }
            "volumes" => {
                if let Value::Array(a) = val {
                    svc.volumes = a.iter().filter_map(|v| convert_volume(v, base)).collect();
                }
            }
            "depends_on" => match val {
                Value::Array(a) => {
                    svc.depends_on = a.iter().filter_map(scalar).map(|s| Dependency { service: s, healthcheck: Vec::new() }).collect();
                }
                Value::Object(m) => {
                    for (dep, cond) in m {
                        let healthy = cond.get("condition").and_then(Value::as_str) == Some("service_healthy");
                        let hc = if healthy {
                            match healths.get(dep).cloned().flatten().and_then(|h| h.exec) {
                                Some(argv) => argv,
                                None => {
                                    report
                                        .approximated
                                        .push(format!("{name}: depends_on {dep} service_healthy has no healthcheck to reuse"));
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        };
                        svc.depends_on.push(Dependency { service: dep.clone(), healthcheck: hc });
                    }
                }
                _ => {}
            },
            "restart" => match scalar(val).as_deref() {
                Some("no") | None => {}
                Some("unless-stopped") => {
                    svc.restart = Some("always".into());
                    report.approximated.push(format!("{name}: restart unless-stopped mapped to always"));
                }
                Some(other) => svc.restart = Some(other.to_string()),
            },
            "cap_add" => svc.cap_add = plain_list(val),
            "cap_drop" => svc.cap_drop = plain_list(val),
            "tmpfs" => svc.tmpfs = plain_list(val),
            "mem_limit" => svc.memory = scalar(val).map(|m| m.to_lowercase()),
            "cpus" => svc.cpus = scalar(val).and_then(|c| c.parse().ok()),
            "healthcheck" => svc.healthcheck = healths.get(name).cloned().flatten(),
            "read_only" => svc.read_only = val.as_bool().unwrap_or(false),
            "stop_signal" => svc.stop_signal = scalar(val),
            "stop_grace_period" => {
                svc.stop_timeout = scalar(val).and_then(|s| collocate_core::size::parse_duration_secs(&s).ok());
            }
            "user" => svc.user = scalar(val),
            "working_dir" => svc.workdir = scalar(val),
            "hostname" => svc.hostname = scalar(val),
            "deploy" => {
                if let Some(o) = val.as_object() {
                    for (k, v) in o {
                        match k.as_str() {
                            "replicas" => svc.replicas = v.as_u64().map_or(1, |n| n.max(1) as u32),
                            "resources" => {
                                let limits = v.get("limits");
                                if let Some(m) = limits.and_then(|l| l.get("memory")).and_then(scalar) {
                                    svc.memory = Some(m.to_lowercase());
                                }
                                if let Some(c) = limits.and_then(|l| l.get("cpus")).and_then(scalar) {
                                    svc.cpus = c.parse().ok();
                                }
                            }
                            other => report.unsupported.push(format!("{name}: deploy.{other}")),
                        }
                    }
                }
            }
            "networks" => report.approximated.push(format!("{name}: networks ignored, every service shares the collocate0 bridge")),
            "container_name" | "labels" | "profiles" | "platform" => report.approximated.push(format!("{name}: {key} ignored")),
            other => report.unsupported.push(format!("{name}: {other}")),
        }
    }
    svc
}

pub fn convert_docker_compose(yaml: &str, project: &str, base_dir: &Path) -> Result<Converted> {
    let doc: Value = serde_saphyr::from_str(yaml).map_err(|e| Error::Parse(e.to_string()))?;
    let services = doc
        .get("services")
        .and_then(Value::as_object)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Invalid("compose file has no services".into()))?;

    let mut report = Report::default();
    let healths: BTreeMap<String, Option<HealthDef>> =
        services.iter().map(|(n, v)| (n.clone(), v.get("healthcheck").and_then(convert_health))).collect();

    let mut file = ComposeFile {
        version: 1,
        project: project.to_string(),
        nodes: BTreeMap::new(),
        services: BTreeMap::new(),
        secrets: BTreeMap::new(),
        configs: BTreeMap::new(),
        registries: BTreeMap::new(),
        loadbalancers: BTreeMap::new(),
    };
    for (name, raw) in services {
        file.services.insert(name.clone(), convert_service(name, raw, base_dir, &mut report, &healths));
    }
    if let Some(obj) = doc.as_object() {
        for key in obj.keys() {
            match key.as_str() {
                "services" | "volumes" | "name" | "version" => {}
                "networks" => {
                    report.approximated.push("networks: user-defined networks are not supported, every service shares one bridge".into())
                }
                other => report.unsupported.push(format!("top-level {other}")),
            }
        }
    }
    file.validate()?;
    Ok(Converted { file, report })
}
