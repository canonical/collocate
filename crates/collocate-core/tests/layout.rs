use collocate_core::layout::{connect_commands, Layout};
use collocate_core::settings::{DaemonSettings, RootModeSetting};
use std::path::Path;

#[test]
fn outside_a_snap_the_fhs_layout_is_used() {
    let l = Layout::from_env(&|_| None);
    assert_eq!(l, Layout::fhs());
    assert_eq!(l.socket(), Path::new("/run/collocate/collocate.sock"));
    assert_eq!(l.config, Path::new("/etc/collocate/collocated.toml"));
    assert_eq!(l.relay, "collocate-relay");
    assert!(!l.snap);
}

#[test]
fn inside_a_snap_everything_lives_under_snap_common() {
    let env = |k: &str| match k {
        "SNAP" => Some("/snap/collocate/x5".to_string()),
        "SNAP_COMMON" => Some("/var/snap/collocate/common".to_string()),
        _ => None,
    };
    let l = Layout::from_env(&env);
    assert!(l.snap);
    assert_eq!(l.socket(), Path::new("/var/snap/collocate/common/run/collocate.sock"));
    assert_eq!(l.state_dir, Path::new("/var/snap/collocate/common/state"));
    assert_eq!(l.config, Path::new("/var/snap/collocate/common/collocated.toml"));
    assert_eq!(l.init_src, Path::new("/snap/collocate/x5/bin/collocate-init"));
    assert_eq!(l.cluster_registry, Path::new("/var/snap/collocate/common/cluster.yaml"));
    assert_eq!(l.lxc, "/snap/collocate/x5/bin/lxc");
    assert_eq!(l.relay, "collocate.relay");
    let partial = |k: &str| (k == "SNAP").then(|| "/snap/x".to_string());
    assert_eq!(Layout::from_env(&partial), Layout::fhs());
}

#[test]
fn connect_commands_name_each_plug() {
    assert_eq!(
        connect_commands("collocate", &["a".into(), "b".into()]),
        vec!["sudo snap connect collocate:a".to_string(), "sudo snap connect collocate:b".to_string()]
    );
}

#[test]
fn settings_round_trip_and_validate() {
    let mut s = DaemonSettings::default();
    assert_eq!(DaemonSettings::from_toml(&s.to_toml().unwrap()).unwrap(), s);
    assert!(!s.to_toml().unwrap().contains("defaults"));
    s.defaults.memory = Some("512m".into());
    s.node_name = Some("edge-1".into());
    s.root_mode = RootModeSetting::FuseOverlay;
    let text = s.to_toml().unwrap();
    assert!(text.contains("root_mode = \"fuse-overlay\""), "{text}");
    assert_eq!(DaemonSettings::from_toml(&text).unwrap(), s);
    s.validate().unwrap();
    for bad in [
        DaemonSettings { bridge: "a-very-long-bridge-name".into(), ..DaemonSettings::default() },
        DaemonSettings { bridge: "br 0".into(), ..DaemonSettings::default() },
        DaemonSettings { group: String::new(), ..DaemonSettings::default() },
        DaemonSettings { node_name: Some("no/slash".into()), ..DaemonSettings::default() },
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
    let mut d = DaemonSettings::default();
    d.defaults.memory = Some("lots".into());
    assert!(d.validate().is_err());
    d.defaults.memory = None;
    d.defaults.cpus = Some(0.0);
    assert!(d.validate().is_err());
    assert!(DaemonSettings::from_toml("state_dir = \"/x\"").is_err());
    assert_eq!(RootModeSetting::parse("bind-ro").unwrap(), RootModeSetting::BindRo);
    assert!(RootModeSetting::parse("zfs").is_err());
}
