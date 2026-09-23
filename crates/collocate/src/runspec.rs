use crate::cli::RunArgs;
use collocate_compose::build::signal_number;
use collocate_core::net::{Publish, Volume};
use collocate_core::size::parse_size;
use collocate_core::spec::{HostEntry, Mount, RestartPolicy, RootSource, Series, Spec};
use collocate_core::{Error, Result};
use collocate_image::config::{spec_from_image, ImageMeta, RunOverrides};
use collocate_sys::caps::CapSet;
use std::path::Path;

fn parse_env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.to_string())))
        .collect()
}

fn set_env(env: &mut Vec<(String, String)>, k: String, v: String) {
    match env.iter_mut().find(|(ek, _)| *ek == k) {
        Some(slot) => slot.1 = v,
        None => env.push((k, v)),
    }
}

fn parse_restart(r: &str) -> Result<RestartPolicy> {
    match r {
        "no" => Ok(RestartPolicy::No),
        "always" => Ok(RestartPolicy::Always),
        "on-failure" => Ok(RestartPolicy::OnFailure { max: 0 }),
        other => other
            .strip_prefix("on-failure:")
            .and_then(|n| n.parse().ok())
            .map(|max| RestartPolicy::OnFailure { max })
            .ok_or_else(|| Error::Invalid(format!("invalid restart policy {other}"))),
    }
}

fn parse_tmpfs(spec: &str) -> Result<Mount> {
    let (dst, opts) = spec.split_once(':').unwrap_or((spec, ""));
    let mut size = None;
    for opt in opts.split(',').filter(|o| !o.is_empty()) {
        if let Some(v) = opt.strip_prefix("size=") {
            size = Some(parse_size(v)?);
        }
    }
    Ok(Mount::Tmpfs { dst: dst.to_string(), size })
}

pub fn build_spec(
    a: &RunArgs,
    default_series: Series,
    host_env: &dyn Fn(&str) -> Option<String>,
    read_file: &dyn Fn(&Path) -> Result<String>,
    lookup_image: &dyn Fn(&str) -> Result<ImageMeta>,
) -> Result<Spec> {
    let name = a.name.clone().unwrap_or_default();
    let mut spec = match &a.image {
        Some(image) => {
            let meta = lookup_image(image)?;
            let ov = RunOverrides {
                command: a.command.clone(),
                entrypoint: a.entrypoint.as_ref().map(|e| e.split_whitespace().map(String::from).collect()),
                env: Vec::new(),
                user: None,
                workdir: None,
                publish_exposed: a.publish_exposed,
            };
            let mut s = spec_from_image(&meta, &ov)?;
            s.name = name.clone();
            s.hostname = name.clone();
            s
        }
        None => {
            if a.command.is_empty() {
                return Err(Error::Invalid("a command is required after --".into()));
            }
            let series = match &a.series {
                Some(s) => Series::parse(s)?,
                None => default_series,
            };
            let mut argv: Vec<String> = a.entrypoint.iter().flat_map(|e| e.split_whitespace().map(String::from)).collect();
            argv.extend(a.command.clone());
            Spec::new(&name, RootSource::Base { series, build_id: "latest".into() }, argv)
        }
    };
    spec.persistent = a.persistent;
    if let Some(h) = &a.hostname {
        spec.hostname = h.clone();
    }

    for f in &a.env_file {
        for (k, v) in parse_env_file(&read_file(f)?) {
            set_env(&mut spec.process.env, k, v);
        }
    }
    for e in &a.env {
        match e.split_once('=') {
            Some((k, v)) => set_env(&mut spec.process.env, k.to_string(), v.to_string()),
            None => {
                if let Some(v) = host_env(e) {
                    set_env(&mut spec.process.env, e.clone(), v);
                }
            }
        }
    }
    if let Some(u) = &a.user {
        spec.process.user = u.clone();
    }
    if let Some(w) = &a.workdir {
        spec.process.workdir = w.clone();
    }
    if let Some(s) = &a.stop_signal {
        spec.process.stop_signal = signal_number(s)?;
    }
    if let Some(t) = a.stop_timeout {
        spec.process.stop_timeout_secs = t;
    }

    if let Some(c) = a.cpus {
        spec.limits.cpus_milli = Some((c * 1000.0).round() as u32);
    }
    spec.limits.cpu_weight = a.cpu_weight;
    if let Some(m) = &a.memory {
        spec.limits.memory = Some(parse_size(m)?);
    }
    if let Some(s) = &a.swap {
        spec.limits.swap = Some(parse_size(s)?);
    }
    if let Some(p) = a.pids_max {
        spec.limits.pids_max = p;
    }

    for v in &a.volume {
        let vol = Volume::parse(v)?;
        spec.mounts.push(if vol.is_named() {
            Mount::Volume { name: vol.src, dst: vol.dst }
        } else {
            Mount::Bind { src: vol.src, dst: vol.dst, ro: vol.ro }
        });
    }
    for s in &a.secret {
        let (name, dst) = s.split_once(':').map_or((s.as_str(), None), |(n, d)| (n, Some(d)));
        spec.mounts.push(Mount::Secret { name: name.to_string(), dst: dst.map_or_else(|| format!("/run/secrets/{name}"), String::from) });
    }
    for t in &a.tmpfs {
        spec.mounts.push(parse_tmpfs(t)?);
    }
    for p in &a.publish {
        spec.net.publish.push(Publish::parse(p)?);
    }
    for d in &a.dns {
        spec.dns.push(d.parse().map_err(|_| Error::Invalid(format!("invalid dns address {d}")))?);
    }
    if let Some(r) = &a.restart {
        spec.restart = parse_restart(r)?;
    }
    spec.read_only_rootfs = a.read_only;
    spec.caps.add = a.cap_add.clone();
    spec.caps.drop = a.cap_drop.clone();
    CapSet::with_policy(&spec.caps.add, &spec.caps.drop)?;
    spec.labels.project = a.project.clone();
    for l in &a.label {
        let (k, v) = l.split_once('=').ok_or_else(|| Error::Invalid(format!("label {l:?} needs the form key=value")))?;
        spec.labels.extra.insert(k.to_string(), v.to_string());
    }
    let _ = HostEntry { name: String::new(), addr: std::net::Ipv4Addr::UNSPECIFIED.into() };
    Ok(spec)
}
