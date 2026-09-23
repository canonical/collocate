use collocate_core::net::{Proto, Publish};
use collocate_core::spec::{HealthKind, Healthcheck, ImageKind, RootSource, Spec};
use collocate_core::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn null_default<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> std::result::Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RawHealth {
    #[serde(default, deserialize_with = "null_default")]
    test: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    interval: u64,
    #[serde(default, deserialize_with = "null_default")]
    timeout: u64,
    #[serde(default, deserialize_with = "null_default")]
    retries: u32,
    #[serde(default, deserialize_with = "null_default")]
    start_period: u64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RawConfig {
    #[serde(default, deserialize_with = "null_default")]
    entrypoint: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    cmd: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    env: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    working_dir: String,
    #[serde(default, deserialize_with = "null_default")]
    user: String,
    #[serde(default, deserialize_with = "null_default")]
    exposed_ports: BTreeMap<String, serde_json::Value>,
    #[serde(default, deserialize_with = "null_default")]
    volumes: BTreeMap<String, serde_json::Value>,
    #[serde(default, deserialize_with = "null_default")]
    stop_signal: String,
    #[serde(default)]
    healthcheck: Option<RawHealth>,
}

#[derive(Deserialize)]
struct RawRootfs {
    #[serde(default)]
    diff_ids: Vec<String>,
}

#[derive(Deserialize)]
struct RawImage {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    config: RawConfig,
    rootfs: RawRootfs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageHealth {
    pub test: Vec<String>,
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub retries: u32,
    pub start_period_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: String,
    pub user: String,
    pub exposed: Vec<String>,
    pub volumes: Vec<String>,
    pub stop_signal: String,
    pub healthcheck: Option<ImageHealth>,
}

impl ImageConfig {
    pub fn exposed_ports(&self) -> Vec<(u16, String)> {
        let mut out: Vec<(u16, String)> = self
            .exposed
            .iter()
            .filter_map(|p| {
                let (port, proto) = p.split_once('/').unwrap_or((p.as_str(), "tcp"));
                Some((port.parse().ok()?, proto.to_string()))
            })
            .collect();
        out.sort();
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMeta {
    pub name: String,
    pub digest: String,
    pub layers: Vec<String>,
    pub config: ImageConfig,
    #[serde(default)]
    pub kind: ImageKind,
}

fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

pub fn parse_config(name: &str, digest: &str, json: &str) -> Result<ImageMeta> {
    let raw: RawImage = serde_json::from_str(json)?;
    if raw.os != "linux" {
        return Err(Error::Invalid(format!("image {name} targets os {:?}, only linux is supported", raw.os)));
    }
    if !raw.architecture.is_empty() && raw.architecture != host_arch() {
        return Err(Error::Invalid(format!("image {name} is built for {}, this host is {}", raw.architecture, host_arch())));
    }
    let c = raw.config;
    Ok(ImageMeta {
        name: name.to_string(),
        digest: digest.to_string(),
        layers: raw.rootfs.diff_ids,
        config: ImageConfig {
            entrypoint: c.entrypoint,
            cmd: c.cmd,
            env: c.env,
            working_dir: c.working_dir,
            user: c.user,
            exposed: c.exposed_ports.into_keys().collect(),
            volumes: c.volumes.into_keys().collect(),
            stop_signal: c.stop_signal,
            healthcheck: c.healthcheck.map(|h| ImageHealth {
                test: h.test,
                interval_secs: h.interval / 1_000_000_000,
                timeout_secs: h.timeout / 1_000_000_000,
                retries: h.retries,
                start_period_secs: h.start_period / 1_000_000_000,
            }),
        },
        kind: ImageKind::Oci,
    })
}

#[derive(Debug, Clone, Default)]
pub struct RunOverrides {
    pub command: Vec<String>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    pub user: Option<String>,
    pub workdir: Option<String>,
    pub publish_exposed: bool,
}

fn signal_number(name: &str) -> Option<i32> {
    if let Ok(n) = name.parse() {
        return Some(n);
    }
    let upper = name.to_ascii_uppercase();
    Some(match upper.strip_prefix("SIG").unwrap_or(&upper) {
        "HUP" => 1,
        "INT" => 2,
        "QUIT" => 3,
        "KILL" => 9,
        "USR1" => 10,
        "USR2" => 12,
        "TERM" => 15,
        "WINCH" => 28,
        _ => return None,
    })
}

pub fn spec_from_image(meta: &ImageMeta, ov: &RunOverrides) -> Result<Spec> {
    let entry = ov.entrypoint.clone().unwrap_or_else(|| meta.config.entrypoint.clone());
    let args = if !ov.command.is_empty() {
        ov.command.clone()
    } else if ov.entrypoint.is_some() {
        Vec::new()
    } else {
        meta.config.cmd.clone()
    };
    let argv: Vec<String> = entry.into_iter().chain(args).collect();
    if argv.is_empty() {
        return Err(Error::InvalidSpec(format!("image {} has no command; supply one after --", meta.name)));
    }
    let mut spec = Spec::new("", RootSource::Oci { digest: meta.digest.clone(), layers: meta.layers.clone() }, argv);
    spec.image_kind = meta.kind;

    for e in &meta.config.env {
        if let Some((k, v)) = e.split_once('=') {
            spec.process.env.push((k.to_string(), v.to_string()));
        }
    }
    for (k, v) in &ov.env {
        match spec.process.env.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.clone(),
            None => spec.process.env.push((k.clone(), v.clone())),
        }
    }
    if let Some(u) = ov.user.clone().or_else(|| Some(meta.config.user.clone()).filter(|u| !u.is_empty())) {
        spec.process.user = u;
    }
    if let Some(w) = ov.workdir.clone().or_else(|| Some(meta.config.working_dir.clone()).filter(|w| !w.is_empty())) {
        spec.process.workdir = w;
    }
    if let Some(sig) = signal_number(&meta.config.stop_signal) {
        spec.process.stop_signal = sig;
    }
    if let Some(h) = &meta.config.healthcheck {
        let exec = match h.test.first().map(String::as_str) {
            Some("CMD") => Some(h.test[1..].to_vec()),
            Some("CMD-SHELL") => Some(vec!["/bin/sh".to_string(), "-c".to_string(), h.test.get(1).cloned().unwrap_or_default()]),
            _ => None,
        };
        if let Some(argv) = exec {
            spec.healthcheck = Some(Healthcheck {
                kind: HealthKind::Exec { argv },
                interval_secs: if h.interval_secs == 0 { 30 } else { h.interval_secs },
                timeout_secs: if h.timeout_secs == 0 { 30 } else { h.timeout_secs },
                retries: if h.retries == 0 { 3 } else { h.retries },
                start_period_secs: h.start_period_secs,
            });
        }
    }
    if spec.healthcheck.is_none() && meta.kind == ImageKind::Pebble {
        spec.healthcheck = Some(Healthcheck {
            kind: HealthKind::Pebble { level: None },
            interval_secs: 10,
            timeout_secs: 5,
            retries: 3,
            start_period_secs: 0,
        });
    }
    if ov.publish_exposed {
        for (port, proto) in meta.config.exposed_ports() {
            let proto = if proto == "udp" { Proto::Udp } else { Proto::Tcp };
            spec.net.publish.push(Publish { host: port, container: port, proto });
        }
    }
    Ok(spec)
}

pub struct ImageStore {
    root: PathBuf,
}

fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c.to_string() } else { format!("%{:02x}", c as u32) })
        .collect()
}

impl ImageStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        ImageStore { root: root.as_ref().to_path_buf() }
    }

    pub fn layers_dir(&self) -> PathBuf {
        self.root.join("layers")
    }

    pub fn layer_dir(&self, diff_id: &str) -> PathBuf {
        self.layers_dir().join(diff_id.replace(':', "-"))
    }

    fn meta_dir(&self) -> PathBuf {
        self.root.join("oci")
    }

    fn meta_path(&self, name: &str) -> PathBuf {
        self.meta_dir().join(format!("{}.json", file_name(name)))
    }

    pub fn put(&self, meta: &ImageMeta) -> Result<()> {
        fs::create_dir_all(self.meta_dir())?;
        fs::write(self.meta_path(&meta.name), serde_json::to_vec_pretty(meta)?)?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<ImageMeta> {
        match fs::read(self.meta_path(name)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!("image {name}"))),
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self) -> Result<Vec<ImageMeta>> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(self.meta_dir()) {
            for e in rd.flatten() {
                if let Ok(b) = fs::read(e.path()) {
                    if let Ok(m) = serde_json::from_slice::<ImageMeta>(&b) {
                        out.push(m);
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        match fs::remove_file(self.meta_path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound(format!("image {name}"))),
            Err(e) => Err(e.into()),
        }
    }

    pub fn gc(&self) -> Result<Vec<String>> {
        let referenced: std::collections::HashSet<String> =
            self.list()?.into_iter().flat_map(|m| m.layers).map(|l| l.replace(':', "-")).collect();
        let mut removed = Vec::new();
        if let Ok(rd) = fs::read_dir(self.layers_dir()) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.starts_with('.') && !referenced.contains(&name) {
                    fs::remove_dir_all(e.path())?;
                    removed.push(name);
                }
            }
        }
        removed.sort();
        Ok(removed)
    }
}
