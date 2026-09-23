mod common;
use collocate_compose::build::{build_spec, BuildCtx};
use collocate_compose::model::ComposeFile;
use collocate_core::net::Proto;
use collocate_core::spec::{HealthKind, ImageKind, Mount, RestartPolicy, RootSource, Series};
use collocate_image::config::{ImageConfig, ImageMeta};
use collocate_image::pull::PullPolicy;
use std::cell::RefCell;
use collocate_core::Error;
use common::FULL;
use std::collections::HashMap;
use std::net::Ipv4Addr;

struct Fixture {
    file: ComposeFile,
    secrets: HashMap<String, String>,
    addresses: HashMap<String, Vec<Ipv4Addr>>,
    lbs: HashMap<String, Ipv4Addr>,
}

fn fixture() -> Fixture {
    let mut secrets = HashMap::new();
    secrets.insert("db_password".to_string(), "hunter2".to_string());
    secrets.insert("registry_user".to_string(), "bot".to_string());
    let mut addresses = HashMap::new();
    addresses.insert("db".to_string(), vec![Ipv4Addr::new(172, 30, 0, 2)]);
    addresses.insert("cache".to_string(), vec![Ipv4Addr::new(172, 30, 0, 3)]);
    Fixture { file: ComposeFile::load(FULL).unwrap(), secrets, addresses, lbs: HashMap::new() }
}

fn fake_image(r: &str) -> ImageMeta {
    let rock = r.starts_with("rock");
    ImageMeta {
        name: r.to_string(),
        digest: format!("sha256:{}", r.len()),
        layers: vec!["sha256:l1".to_string(), "sha256:l2".to_string()],
        config: ImageConfig {
            entrypoint: if rock { vec!["/bin/pebble".into(), "enter".into()] } else { Vec::new() },
            cmd: if rock { Vec::new() } else { vec!["redis-server".into()] },
            env: vec!["PATH=/usr/bin:/bin".into(), "MODE=image".into()],
            working_dir: "/data".into(),
            ..ImageConfig::default()
        },
        kind: if rock { ImageKind::Pebble } else { ImageKind::Oci },
    }
}

fn build(fx: &Fixture, service: &str, replica: u32) -> Result<collocate_core::spec::Spec, Error> {
    build_with_policies(fx, service, replica, &RefCell::new(Vec::new()))
}

fn build_with_policies(fx: &Fixture, service: &str, replica: u32, seen: &RefCell<Vec<PullPolicy>>) -> Result<collocate_core::spec::Spec, Error> {
    let base = |s: Series| Ok(format!("{}-build", s.dir_name()));
    let oci = |r: &str, p: PullPolicy| {
        seen.borrow_mut().push(p);
        Ok(fake_image(r))
    };
    let cfg = |n: &str| Ok(format!("/run/collocate/configs/{n}"));
    let ctx =
        BuildCtx { secrets: &fx.secrets, addresses: &fx.addresses, lb_addresses: &fx.lbs, base_build: &base, oci: &oci, config_path: &cfg };
    build_spec(&fx.file, service, replica, &ctx)
}

#[test]
fn identity_naming_and_labels() {
    let fx = fixture();
    let s = build(&fx, "server", 1).unwrap();
    assert_eq!(s.name, "myapp-server-2");
    assert_eq!(s.hostname, "server-2");
    assert_eq!(s.labels.project.as_deref(), Some("myapp"));
    assert_eq!(s.labels.service.as_deref(), Some("server"));
    assert_eq!(s.labels.replica, Some(1));
    assert_eq!(s.labels.node.as_deref(), Some("edge-2"));
    assert!(s.labels.revision.is_some());
    let single = build(&fx, "db", 0).unwrap();
    assert_eq!(single.name, "myapp-db-1");
    assert_eq!(single.hostname, "db");
}

#[test]
fn replicas_share_a_revision() {
    let fx = fixture();
    let a = build(&fx, "server", 0).unwrap();
    let b = build(&fx, "server", 1).unwrap();
    assert_eq!(a.labels.revision, b.labels.revision);
}

#[test]
fn root_comes_from_series_or_oci() {
    let fx = fixture();
    match build(&fx, "db", 0).unwrap().root {
        RootSource::Base { series, build_id } => {
            assert_eq!(series, Series::Noble);
            assert_eq!(build_id, "noble-build");
        }
        other => panic!("{other:?}"),
    }
    match build(&fx, "cache", 0).unwrap().root {
        RootSource::Oci { layers, .. } => assert_eq!(layers.len(), 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn limits_and_persistence() {
    let fx = fixture();
    let db = build(&fx, "db", 0).unwrap();
    assert!(db.persistent);
    assert_eq!(db.limits.memory, Some(1 << 30));
    assert_eq!(db.limits.cpus_milli, Some(1000));
    let server = build(&fx, "server", 0).unwrap();
    assert_eq!(server.limits.memory, Some(512 << 20));
    assert_eq!(server.limits.ulimits[0].name, "nofile");
    assert_eq!(server.limits.ulimits[0].soft, 65536);
}

#[test]
fn templates_are_resolved_in_env() {
    let fx = fixture();
    let s = build(&fx, "server", 0).unwrap();
    let env: HashMap<_, _> = s.process.env.iter().cloned().collect();
    assert_eq!(env["DATABASE_URL"], "postgres://appuser:hunter2@172.30.0.2:5432/appdb");
    assert_eq!(env["REDIS_URL"], "redis://172.30.0.3:6379");
}

#[test]
fn mounts_cover_volumes_tmpfs_secrets_and_configs() {
    let fx = fixture();
    let s = build(&fx, "server", 0).unwrap();
    assert!(s.mounts.contains(&Mount::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) }));
    assert!(s.mounts.contains(&Mount::Secret { name: "db_password".into(), dst: "/run/secrets/db_password".into() }));
    assert!(s
        .mounts
        .contains(&Mount::Config { src: "/run/collocate/configs/app_settings".into(), dst: "/etc/myapp/settings.yaml".into() }));
    let db = build(&fx, "db", 0).unwrap();
    assert!(db.mounts.contains(&Mount::Bind { src: "/var/lib/myapp/db-data".into(), dst: "/var/lib/postgresql/data".into(), ro: false }));
}

#[test]
fn process_network_restart_and_health() {
    let fx = fixture();
    let s = build(&fx, "server", 0).unwrap();
    assert_eq!(s.process.argv, vec!["/usr/bin/myserver", "--listen", "8080"]);
    assert_eq!(s.process.stop_signal, 2);
    assert_eq!(s.process.stop_timeout_secs, 30);
    assert_eq!(s.restart, RestartPolicy::OnFailure { max: 3 });
    assert_eq!(s.net.publish[0].host, 8080);
    assert_eq!(s.net.publish[0].proto, Proto::Tcp);
    let h = s.healthcheck.unwrap();
    assert_eq!(h.kind, HealthKind::Tcp { port: 8080 });
    assert_eq!((h.interval_secs, h.timeout_secs, h.retries), (5, 2, 3));
}

#[test]
fn spec_validates_and_is_reproducible() {
    let fx = fixture();
    let a = build(&fx, "server", 0).unwrap();
    a.validate().unwrap();
    let b = build(&fx, "server", 0).unwrap();
    assert_eq!(a.spec_hash(), b.spec_hash());
}

#[test]
fn changing_a_secret_changes_the_revision() {
    let mut fx = fixture();
    let before = build(&fx, "server", 0).unwrap().labels.revision;
    fx.secrets.insert("db_password".into(), "rotated".into());
    let after = build(&fx, "server", 0).unwrap().labels.revision;
    assert_ne!(before, after);
}

#[test]
fn missing_inputs_fail_cleanly() {
    let mut fx = fixture();
    fx.secrets.clear();
    assert!(build(&fx, "server", 0).is_err());
    let fx = fixture();
    assert!(matches!(build(&fx, "ghost", 0), Err(Error::NotFound(_))));
    assert!(build(&fx, "server", 11).is_err());
}

#[test]
fn stop_signal_names_are_understood() {
    assert_eq!(collocate_compose::build::signal_number("SIGTERM").unwrap(), 15);
    assert_eq!(collocate_compose::build::signal_number("QUIT").unwrap(), 3);
    assert_eq!(collocate_compose::build::signal_number("9").unwrap(), 9);
    assert!(collocate_compose::build::signal_number("SIGNOPE").is_err());
}

fn image_fixture(services: &str) -> Fixture {
    let yaml = format!("version: 1\nproject: rocks\nservices:\n{services}");
    Fixture { file: ComposeFile::load(&yaml).unwrap(), secrets: HashMap::new(), addresses: HashMap::new(), lbs: HashMap::new() }
}

#[test]
fn image_services_inherit_command_env_and_workdir_from_the_image() {
    let fx = image_fixture("  cache:\n    image: redis:7\n    env:\n      MODE: compose\n      EXTRA: \"1\"\n");
    let s = build(&fx, "cache", 0).unwrap();
    assert_eq!(s.process.argv, vec!["redis-server"]);
    assert_eq!(s.process.workdir, "/data");
    assert_eq!(s.name, "rocks-cache-1");
    let env: HashMap<_, _> = s.process.env.iter().cloned().collect();
    assert_eq!(env["MODE"], "compose");
    assert_eq!(env["EXTRA"], "1");
    assert_eq!(env["PATH"], "/usr/bin:/bin");
    assert_eq!(s.process.env.iter().filter(|(k, _)| k == "MODE").count(), 1);
}

#[test]
fn service_command_and_entrypoint_override_the_image() {
    let fx = image_fixture("  cache:\n    image: redis:7\n    command: [\"--port\", \"7000\"]\n");
    assert_eq!(build(&fx, "cache", 0).unwrap().process.argv, vec!["--port", "7000"]);
    let fx = image_fixture("  cache:\n    image: redis:7\n    entrypoint: [\"/bin/other\"]\n");
    assert_eq!(build(&fx, "cache", 0).unwrap().process.argv, vec!["/bin/other"]);
}

#[test]
fn rocks_get_a_pebble_healthcheck_by_default() {
    let fx = image_fixture("  pg:\n    image: rock-postgres:14\n");
    let s = build(&fx, "pg", 0).unwrap();
    assert_eq!(s.image_kind, ImageKind::Pebble);
    assert_eq!(s.process.argv, vec!["/bin/pebble", "enter"]);
    assert_eq!(s.healthcheck.unwrap().kind, HealthKind::Pebble { level: None });
}

#[test]
fn explicit_healthchecks_win_over_the_pebble_default() {
    let fx = image_fixture("  pg:\n    image: rock-postgres:14\n    healthcheck:\n      tcp: 5432\n");
    assert_eq!(build(&fx, "pg", 0).unwrap().healthcheck.unwrap().kind, HealthKind::Tcp { port: 5432 });
    let fx = image_fixture("  pg:\n    image: rock-postgres:14\n    healthcheck:\n      pebble: ready\n");
    assert_eq!(build(&fx, "pg", 0).unwrap().healthcheck.unwrap().kind, HealthKind::Pebble { level: Some("ready".into()) });
    let fx = image_fixture("  pg:\n    image: rock-postgres:14\n    healthcheck:\n      pebble: any\n");
    assert_eq!(build(&fx, "pg", 0).unwrap().healthcheck.unwrap().kind, HealthKind::Pebble { level: None });
}

#[test]
fn pull_policy_reaches_the_resolver() {
    let seen = RefCell::new(Vec::new());
    let fx = image_fixture("  a:\n    image: redis:7\n    pull_policy: always\n  b:\n    image: redis:7\n");
    build_with_policies(&fx, "a", 0, &seen).unwrap();
    build_with_policies(&fx, "b", 0, &seen).unwrap();
    assert_eq!(*seen.borrow(), vec![PullPolicy::Always, PullPolicy::Missing]);
}
