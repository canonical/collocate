use crate::model::{ComposeFile, HealthDef};
use crate::template::{resolve, Context};
use collocate_core::limits::Ulimit;
use collocate_core::net::{Publish, Volume};
use collocate_core::size::{parse_duration_secs, parse_size};
use collocate_core::spec::{HealthKind, Healthcheck, Mount, RestartPolicy, RootSource, Series, Spec};
use collocate_core::{ContainerId, Error, Result};
use collocate_image::config::{spec_from_image, ImageMeta, RunOverrides};
use collocate_image::pull::PullPolicy;
use std::collections::HashMap;
use std::net::Ipv4Addr;

pub type OciResolver<'a> = &'a dyn Fn(&str, PullPolicy) -> Result<ImageMeta>;

pub struct BuildCtx<'a> {
    pub secrets: &'a HashMap<String, String>,
    pub addresses: &'a HashMap<String, Vec<Ipv4Addr>>,
    pub lb_addresses: &'a HashMap<String, Ipv4Addr>,
    pub base_build: &'a dyn Fn(Series) -> Result<String>,
    pub oci: OciResolver<'a>,
    pub config_path: &'a dyn Fn(&str) -> Result<String>,
}

pub fn signal_number(name: &str) -> Result<i32> {
    if let Ok(n) = name.parse::<i32>() {
        return Ok(n);
    }
    let upper = name.to_ascii_uppercase();
    let short = upper.strip_prefix("SIG").unwrap_or(&upper);
    Ok(match short {
        "HUP" => 1,
        "INT" => 2,
        "QUIT" => 3,
        "KILL" => 9,
        "USR1" => 10,
        "USR2" => 12,
        "TERM" => 15,
        "CONT" => 18,
        "STOP" => 19,
        _ => return Err(Error::Invalid(format!("unknown signal {name}"))),
    })
}

fn parse_restart(r: &str) -> RestartPolicy {
    match r {
        "always" => RestartPolicy::Always,
        "on-failure" => RestartPolicy::OnFailure { max: 0 },
        _ => match r.strip_prefix("on-failure:").and_then(|n| n.parse().ok()) {
            Some(max) => RestartPolicy::OnFailure { max },
            None => RestartPolicy::No,
        },
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

fn health(def: &HealthDef, ctx: &Context) -> Result<Healthcheck> {
    let kind = if let Some(port) = def.tcp {
        HealthKind::Tcp { port }
    } else if let Some(h) = &def.http {
        HealthKind::Http { port: h.port, path: h.path.clone() }
    } else if let Some(level) = &def.pebble {
        HealthKind::Pebble { level: Some(level.clone()).filter(|l| l != "any") }
    } else {
        let argv = def.exec.clone().unwrap_or_default();
        HealthKind::Exec { argv: argv.iter().map(|a| resolve(a, ctx)).collect::<Result<_>>()? }
    };
    Ok(Healthcheck {
        kind,
        interval_secs: parse_duration_secs(&def.interval)?,
        timeout_secs: parse_duration_secs(&def.timeout)?,
        retries: def.retries,
        start_period_secs: parse_duration_secs(&def.start_period)?,
    })
}

pub fn build_spec(file: &ComposeFile, service: &str, replica: u32, ctx: &BuildCtx) -> Result<Spec> {
    let svc = file.services.get(service).ok_or_else(|| Error::NotFound(format!("service {service}")))?;
    let max = svc.max_replicas();
    if replica >= svc.index_bound() {
        return Err(Error::Invalid(format!("replica {replica} is out of range for service {service}")));
    }
    let tctx = Context { secrets: ctx.secrets.clone(), addresses: ctx.addresses.clone(), lb_addresses: ctx.lb_addresses.clone() };

    let resolve_all = |args: &[String]| args.iter().map(|a| resolve(a, &tctx)).collect::<Result<Vec<_>>>();
    let name = format!("{}-{}-{}", file.project, service, replica + 1);
    let mut spec = match (&svc.series, &svc.image) {
        (Some(series), _) => {
            let series = Series::parse(series)?;
            let root = RootSource::Base { series, build_id: (ctx.base_build)(series)? };
            Spec::new(&name, root, resolve_all(&[svc.entrypoint.clone(), svc.command.clone()].concat())?)
        }
        (None, Some(image)) => {
            let policy = svc.pull_policy.as_deref().map(PullPolicy::parse).transpose()?.unwrap_or_default();
            let meta = (ctx.oci)(image, policy)?;
            let ov = RunOverrides {
                command: resolve_all(&svc.command)?,
                entrypoint: if svc.entrypoint.is_empty() { None } else { Some(resolve_all(&svc.entrypoint)?) },
                ..RunOverrides::default()
            };
            let mut spec = spec_from_image(&meta, &ov).map_err(|e| Error::InvalidSpec(format!("service {service}: {e}")))?;
            spec.name = name.clone();
            spec
        }
        (None, None) => return Err(Error::InvalidSpec(format!("service {service} has no root"))),
    };
    spec.hostname = if max == 1 { service.to_string() } else { format!("{service}-{}", replica + 1) };
    spec.persistent = svc.persistent;

    for (k, v) in &svc.env {
        let v = resolve(v, &tctx)?;
        match spec.process.env.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v,
            None => spec.process.env.push((k.clone(), v)),
        }
    }
    if let Some(u) = &svc.user {
        spec.process.user = u.clone();
    }
    if let Some(w) = &svc.workdir {
        spec.process.workdir = w.clone();
    }
    if let Some(h) = &svc.hostname {
        spec.hostname = h.clone();
    }
    if let Some(sig) = &svc.stop_signal {
        spec.process.stop_signal = signal_number(sig)?;
    }
    if let Some(t) = svc.stop_timeout {
        spec.process.stop_timeout_secs = t;
    }

    if let Some(c) = svc.cpus {
        spec.limits.cpus_milli = Some((c * 1000.0).round() as u32);
    }
    if let Some(m) = &svc.memory {
        spec.limits.memory = Some(parse_size(m)?);
    }
    for (n, v) in &svc.ulimits {
        spec.limits.ulimits.push(Ulimit { name: n.clone(), soft: *v, hard: *v });
    }

    for v in svc.volume.iter().chain(&svc.volumes) {
        let vol = Volume::parse(v)?;
        spec.mounts.push(if vol.is_named() {
            Mount::Volume { name: vol.src, dst: vol.dst }
        } else {
            Mount::Bind { src: vol.src, dst: vol.dst, ro: vol.ro }
        });
    }
    for t in &svc.tmpfs {
        spec.mounts.push(parse_tmpfs(t)?);
    }
    for s in &svc.secrets {
        spec.mounts.push(Mount::Secret { name: s.clone(), dst: format!("/run/secrets/{s}") });
    }
    for (cfg, dst) in &svc.configs {
        spec.mounts.push(Mount::Config { src: (ctx.config_path)(cfg)?, dst: dst.clone() });
    }

    for p in &svc.publish {
        spec.net.publish.push(Publish::parse(p)?);
    }
    if let Some(r) = &svc.restart {
        spec.restart = parse_restart(r);
    }
    if let Some(h) = &svc.healthcheck {
        spec.healthcheck = Some(health(h, &tctx)?);
    }
    spec.caps.add = svc.cap_add.clone();
    spec.caps.drop = svc.cap_drop.clone();
    spec.read_only_rootfs = svc.read_only;

    spec.labels.project = Some(file.project.clone());
    spec.labels.service = Some(service.to_string());
    spec.labels.node = svc.node.clone();
    spec.labels.replica = Some(replica);

    let mut normalised = spec.clone();
    normalised.id = ContainerId::from_bytes([0; 6]);
    normalised.name = String::new();
    normalised.hostname = String::new();
    normalised.labels.replica = None;
    spec.labels.revision = Some(normalised.spec_hash_hex()[..12].to_string());
    Ok(spec)
}
