use collocated::config::{Config, RootModeSetting};

#[test]
fn defaults_match_the_documented_layout() {
    let c = Config::default();
    assert_eq!(c.state_dir.to_str(), Some("/var/lib/collocate"));
    assert_eq!(c.run_dir.to_str(), Some("/run/collocate"));
    assert_eq!(c.socket().to_str(), Some("/run/collocate/collocate.sock"));
    assert_eq!(c.images_dir().to_str(), Some("/var/lib/collocate/images"));
    assert_eq!(c.subnet, "172.30.0.0/16");
    assert_eq!(c.bridge, "collocate0");
    assert_eq!(c.root_mode, RootModeSetting::Auto);
    assert_eq!(c.init_path.to_str(), Some("/usr/libexec/collocate/collocate-init"));
    assert_eq!(c.cgroup_slice, "collocate.slice");
}

#[test]
fn toml_overrides_selected_fields() {
    let c = Config::from_toml(
        "subnet = \"10.9.0.0/16\"\nroot_mode = \"bind-ro\"\nstate_dir = \"/tmp/s\"\n[defaults]\npids_max = 100\nmemory = \"1g\"\n",
    )
    .unwrap();
    assert_eq!(c.subnet, "10.9.0.0/16");
    assert_eq!(c.root_mode, RootModeSetting::BindRo);
    assert_eq!(c.state_dir.to_str(), Some("/tmp/s"));
    assert_eq!(c.defaults.pids_max, Some(100));
    assert_eq!(c.defaults.memory.as_deref(), Some("1g"));
    assert_eq!(c.bridge, "collocate0");
}

#[test]
fn unknown_keys_and_bad_values_are_rejected() {
    assert!(Config::from_toml("bogus = 1").is_err());
    assert!(Config::from_toml("root_mode = \"weird\"").is_err());
    assert!(Config::from_toml("subnet = \"nope\"").is_err());
}

#[test]
fn defaults_apply_to_unlimited_specs_only() {
    use collocate_core::limits::Limits;
    let c = Config::from_toml("[defaults]\nmemory = \"1g\"\npids_max = 100\ncpus = 2.0\n").unwrap();
    let mut l = Limits::default();
    c.apply_defaults(&mut l).unwrap();
    assert_eq!(l.memory, Some(1 << 30));
    assert_eq!(l.pids_max, 100);
    assert_eq!(l.cpus_milli, Some(2000));
    let mut explicit = Limits { memory: Some(1 << 20), pids_max: 7, ..Limits::default() };
    c.apply_defaults(&mut explicit).unwrap();
    assert_eq!(explicit.memory, Some(1 << 20));
    assert_eq!(explicit.pids_max, 7);
}

#[test]
fn a_missing_config_file_means_uninitialized() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Config::load(&dir.path().join("absent.toml")).unwrap().is_none());
    std::fs::write(dir.path().join("c.toml"), "bridge = \"br9\"\n").unwrap();
    assert_eq!(Config::load(&dir.path().join("c.toml")).unwrap().unwrap().bridge, "br9");
}

#[test]
fn settings_are_carried_between_config_and_daemon_settings() {
    use collocate_core::layout::Layout;
    use collocate_core::settings::DaemonSettings;
    let layout = Layout::snap(std::path::Path::new("/snap/collocate/x1"), std::path::Path::new("/var/snap/collocate/common"));
    let s = DaemonSettings {
        subnet: "10.8.0.0/16".into(),
        defaults: collocate_core::settings::Defaults { pids_max: Some(10), ..Default::default() },
        ..DaemonSettings::default()
    };
    let c = Config::from_settings(&layout, &s);
    assert_eq!(c.settings(), s);
    assert_eq!(c.socket().to_str(), Some("/var/snap/collocate/common/run/collocate.sock"));
    assert_eq!(c.init_path.to_str(), Some("/snap/collocate/x1/bin/collocate-init"));
}

#[test]
fn rewriting_the_config_keeps_unrelated_keys_and_drops_stale_settings() {
    use collocate_core::settings::DaemonSettings;
    use collocated::setup::render_config;
    let existing =
        "state_dir = \"/srv/c\"\ninit_path = \"/opt/init\"\nsubnet = \"10.1.0.0/16\"\nnode_name = \"old\"\n[defaults]\nmemory = \"1g\"\n";
    let s = DaemonSettings { subnet: "10.2.0.0/16".into(), ..DaemonSettings::default() };
    let text = render_config(Some(existing), &s).unwrap();
    let c = Config::from_toml(&text).unwrap();
    assert_eq!(c.state_dir.to_str(), Some("/srv/c"));
    assert_eq!(c.init_path.to_str(), Some("/opt/init"));
    assert_eq!(c.subnet, "10.2.0.0/16");
    assert_eq!(c.node_name, None);
    assert_eq!(c.defaults.memory, None);
    let fresh = render_config(None, &s).unwrap();
    assert!(!fresh.contains("state_dir") && !fresh.contains("init_path"), "{fresh}");
}

#[test]
fn subnet_overlaps_with_host_routes_are_reported() {
    use collocate_net::ipam::Subnet;
    use collocated::setup::{overlaps, route_conflicts};
    let a = Subnet::parse("172.30.0.0/16").unwrap();
    assert!(overlaps(&a, "172.30.5.0/24"));
    assert!(overlaps(&a, "172.0.0.0/8"));
    assert!(overlaps(&a, "172.30.1.1"));
    assert!(!overlaps(&a, "172.31.0.0/16"));
    assert!(!overlaps(&a, "garbage"));
    let routes = r#"[{"dst":"default","dev":"eth0"},{"dst":"10.43.57.0/24","dev":"lxdbr0"},{"dst":"172.30.0.0/16","dev":"collocate0"},{"dst":"172.30.9.9","dev":"tun0"}]"#;
    assert_eq!(route_conflicts(routes, &a, &["collocate0"]), vec!["172.30.9.9 on tun0".to_string()]);
    assert!(route_conflicts(routes, &Subnet::parse("10.44.0.0/16").unwrap(), &[]).is_empty());
    assert!(route_conflicts("not json", &a, &[]).is_empty());
}

#[test]
fn the_init_binary_is_staged_by_content_and_old_copies_are_pruned() {
    use collocated::setup::stage_init;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("init");
    let mut cfg = Config { state_dir: dir.path().join("state"), init_path: src.clone(), ..Config::default() };
    let mut staged = Vec::new();
    for i in 0..5 {
        std::fs::write(&src, format!("binary {i}")).unwrap();
        let p = stage_init(&cfg).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), format!("binary {i}"));
        staged.push(p);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(stage_init(&cfg).unwrap(), staged[4]);
    std::fs::remove_file(&src).unwrap();
    assert!(staged[4].is_file());
    let left = std::fs::read_dir(dir.path().join("state/bin")).unwrap().count();
    assert_eq!(left, 3);
    cfg.init_path = dir.path().join("missing");
    assert!(stage_init(&cfg).is_err());
}
