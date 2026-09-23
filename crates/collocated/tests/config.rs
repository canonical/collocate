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
