use collocate_core::spec::{HealthKind, RootSource};
use collocate_image::config::{parse_config, spec_from_image, ImageMeta, RunOverrides};

const CONFIG: &str = r#"{
  "architecture": "amd64", "os": "linux",
  "config": {
    "Env": ["PATH=/usr/local/bin:/usr/bin", "PGDATA=/var/lib/postgresql/data", "LANG=C"],
    "Entrypoint": ["docker-entrypoint.sh"],
    "Cmd": ["postgres"],
    "WorkingDir": "/srv",
    "User": "postgres",
    "ExposedPorts": {"5432/tcp": {}, "53/udp": {}},
    "Volumes": {"/var/lib/postgresql/data": {}},
    "StopSignal": "SIGINT",
    "Healthcheck": {"Test": ["CMD-SHELL", "pg_isready"], "Interval": 30000000000, "Timeout": 5000000000, "Retries": 4}
  },
  "rootfs": {"type": "layers", "diff_ids": ["sha256:aaa", "sha256:bbb"]}
}"#;

fn meta() -> ImageMeta {
    parse_config("postgres:16", "sha256:cfg", CONFIG).unwrap()
}

#[test]
fn config_fields_are_parsed() {
    let m = meta();
    assert_eq!(m.layers, vec!["sha256:aaa", "sha256:bbb"]);
    assert_eq!(m.config.entrypoint, vec!["docker-entrypoint.sh"]);
    assert_eq!(m.config.cmd, vec!["postgres"]);
    assert_eq!(m.config.working_dir, "/srv");
    assert_eq!(m.config.exposed_ports(), vec![(53, "udp".to_string()), (5432, "tcp".to_string())]);
    assert_eq!(m.config.volumes, vec!["/var/lib/postgresql/data".to_string()]);
}

#[test]
fn only_linux_matching_architectures_are_accepted() {
    let arm = CONFIG.replace("amd64", "arm64");
    let expected_ok = parse_config("x", "d", &arm);
    let host = std::env::consts::ARCH;
    if host == "x86_64" {
        assert!(expected_ok.is_err());
    }
    assert!(parse_config("x", "d", &CONFIG.replace("linux", "windows")).is_err());
}

#[test]
fn entrypoint_is_a_fixed_prefix_and_cmd_a_default_suffix() {
    let s = spec_from_image(&meta(), &RunOverrides::default()).unwrap();
    assert_eq!(s.process.argv, vec!["docker-entrypoint.sh", "postgres"]);
    let s = spec_from_image(&meta(), &RunOverrides { command: vec!["-c".into(), "fsync=off".into()], ..RunOverrides::default() }).unwrap();
    assert_eq!(s.process.argv, vec!["docker-entrypoint.sh", "-c", "fsync=off"]);
}

#[test]
fn an_entrypoint_override_replaces_the_prefix_and_drops_the_default_cmd() {
    let s = spec_from_image(&meta(), &RunOverrides { entrypoint: Some(vec!["/bin/sh".into()]), ..RunOverrides::default() }).unwrap();
    assert_eq!(s.process.argv, vec!["/bin/sh"]);
    let s = spec_from_image(
        &meta(),
        &RunOverrides { entrypoint: Some(vec!["/bin/sh".into()]), command: vec!["-c".into(), "id".into()], ..RunOverrides::default() },
    )
    .unwrap();
    assert_eq!(s.process.argv, vec!["/bin/sh", "-c", "id"]);
}

#[test]
fn env_merges_with_overrides_winning() {
    let s = spec_from_image(
        &meta(),
        &RunOverrides { env: vec![("LANG".into(), "en".into()), ("NEW".into(), "1".into())], ..RunOverrides::default() },
    )
    .unwrap();
    let env: std::collections::HashMap<_, _> = s.process.env.iter().cloned().collect();
    assert_eq!(env["LANG"], "en");
    assert_eq!(env["NEW"], "1");
    assert_eq!(env["PGDATA"], "/var/lib/postgresql/data");
    assert_eq!(s.process.env.iter().filter(|(k, _)| k == "LANG").count(), 1);
}

#[test]
fn user_workdir_stop_signal_and_root_come_from_the_image() {
    let s = spec_from_image(&meta(), &RunOverrides::default()).unwrap();
    assert_eq!(s.process.user, "postgres");
    assert_eq!(s.process.workdir, "/srv");
    assert_eq!(s.process.stop_signal, 2);
    match s.root {
        RootSource::Oci { digest, layers } => {
            assert_eq!(digest, "sha256:cfg");
            assert_eq!(layers, vec!["sha256:aaa", "sha256:bbb"]);
        }
        _ => panic!("expected an OCI root"),
    }
    let o = spec_from_image(&meta(), &RunOverrides { user: Some("root".into()), workdir: Some("/".into()), ..RunOverrides::default() })
        .unwrap();
    assert_eq!((o.process.user.as_str(), o.process.workdir.as_str()), ("root", "/"));
}

#[test]
fn healthchecks_are_translated() {
    let s = spec_from_image(&meta(), &RunOverrides::default()).unwrap();
    let h = s.healthcheck.unwrap();
    assert_eq!(h.kind, HealthKind::Exec { argv: vec!["/bin/sh".into(), "-c".into(), "pg_isready".into()] });
    assert_eq!((h.interval_secs, h.timeout_secs, h.retries), (30, 5, 4));
}

#[test]
fn exposed_ports_are_only_published_on_request() {
    let s = spec_from_image(&meta(), &RunOverrides::default()).unwrap();
    assert!(s.net.publish.is_empty());
    let s = spec_from_image(&meta(), &RunOverrides { publish_exposed: true, ..RunOverrides::default() }).unwrap();
    let mut got: Vec<String> = s.net.publish.iter().map(ToString::to_string).collect();
    got.sort();
    assert_eq!(got, vec!["53:53/udp", "5432:5432/tcp"]);
}

#[test]
fn declared_volumes_are_reported_for_the_caller_to_handle() {
    let m = meta();
    let s = spec_from_image(&m, &RunOverrides::default()).unwrap();
    assert!(s.mounts.is_empty());
    assert_eq!(m.config.volumes.len(), 1);
}

#[test]
fn an_image_without_a_command_needs_one_from_the_user() {
    let cfg = r#"{"architecture":"amd64","os":"linux","config":{},"rootfs":{"type":"layers","diff_ids":["sha256:a"]}}"#;
    let m = parse_config("bare", "d", cfg).unwrap();
    assert!(spec_from_image(&m, &RunOverrides::default()).is_err());
    assert!(spec_from_image(&m, &RunOverrides { command: vec!["/app".into()], ..RunOverrides::default() }).is_ok());
}
