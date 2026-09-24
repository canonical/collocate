use collocate_cluster::preseed::{ClusterPreseed, LxdTarget, Mode, NodeRecord, Preseed, Registry};
use collocate_cluster::provision::{
    cluster_members, instance_states, launch_args, node_preseed, plan, reports_initialized, Install, Observed, StepKind, PROFILE,
};
use collocate_core::layout::DAEMON_PLUGS;
use collocate_core::settings::DaemonSettings;
use std::collections::BTreeMap;

fn nodes(names: &[&str]) -> BTreeMap<String, NodeRecord> {
    names.iter().map(|n| (n.to_string(), NodeRecord::default())).collect()
}

fn kinds_for(steps: &[collocate_cluster::provision::Step], node: &str) -> Vec<StepKind> {
    steps.iter().filter(|s| s.node == node).map(|s| s.kind).collect()
}

#[test]
fn install_modes_parse_and_round_trip() {
    assert_eq!(Install::parse("channel:latest/edge").unwrap(), Install::Channel("latest/edge".into()));
    assert_eq!(Install::parse("file:/tmp/c.snap").unwrap(), Install::SnapFile("/tmp/c.snap".into()));
    assert_eq!(Install::parse("deb:/a.deb,/b.deb").unwrap(), Install::Deb(vec!["/a.deb".into(), "/b.deb".into()]));
    for bad in ["latest/stable", "channel:", "tarball:/x"] {
        assert!(Install::parse(bad).is_err(), "{bad}");
    }
    let i = Install::parse("deb:/a.deb,/b.deb").unwrap();
    assert_eq!(Install::parse(&i.label()).unwrap(), i);
    assert_eq!(i.relay(), "collocate-relay");
    assert_eq!(Install::Channel("x".into()).relay(), "collocate.relay");
}

#[test]
fn fresh_nodes_get_the_profile_launch_install_connect_and_init() {
    let steps = plan(
        &LxdTarget::default(),
        &nodes(&["n1", "n2"]),
        "ubuntu:24.04",
        &Install::Channel("latest/stable".into()),
        &DaemonSettings::default(),
        &Observed::default(),
    )
    .unwrap();
    assert_eq!(steps.iter().filter(|s| s.kind == StepKind::Profile).count(), 2);
    for n in ["n1", "n2"] {
        let k = kinds_for(&steps, n);
        assert_eq!(k[0], StepKind::Launch);
        assert_eq!(k[1], StepKind::WaitReady);
        assert_eq!(k[2], StepKind::Install);
        assert_eq!(k.iter().filter(|x| **x == StepKind::Connect).count(), DAEMON_PLUGS.len());
        assert_eq!(*k.last().unwrap(), StepKind::Init);
    }
    let install = steps.iter().find(|s| s.node == "n1" && s.kind == StepKind::Install).unwrap();
    assert_eq!(install.args, ["exec", "n1", "--", "snap", "install", "collocate", "--channel=latest/stable"]);
    let init = steps.iter().find(|s| s.node == "n2" && s.kind == StepKind::Init).unwrap();
    assert_eq!(init.args, ["exec", "n2", "--", "collocate", "init", "--preseed"]);
    let seeded = Preseed::parse(init.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(seeded.mode, Mode::Local);
    assert_eq!(seeded.daemon.node_name.as_deref(), Some("n2"));
}

#[test]
fn provisioning_is_idempotent_and_resumes_partial_work() {
    let target = LxdTarget::default();
    let ns = nodes(&["n1", "n2", "n3"]);
    let install = Install::Channel("latest/stable".into());
    let mut o = Observed { profile: true, ..Observed::default() };
    o.instances.insert("n1".into(), "Running".into());
    o.instances.insert("n2".into(), "Running".into());
    o.instances.insert("n3".into(), "Stopped".into());
    o.installed.insert("n1".into());
    o.initialized.insert("n1".into());
    o.installed.insert("n2".into());
    let steps = plan(&target, &ns, "ubuntu:24.04", &install, &DaemonSettings::default(), &o).unwrap();
    assert!(kinds_for(&steps, "n1").is_empty(), "{steps:?}");
    let n2 = kinds_for(&steps, "n2");
    assert!(!n2.contains(&StepKind::Install) && !n2.contains(&StepKind::Launch));
    assert_eq!(*n2.last().unwrap(), StepKind::Init);
    let n3 = kinds_for(&steps, "n3");
    assert_eq!(n3[0], StepKind::Start);
    assert!(n3.contains(&StepKind::Install));
    assert!(steps.iter().all(|s| s.kind != StepKind::Profile && s.kind != StepKind::Launch));
    for n in ["n2", "n3"] {
        o.installed.insert(n.into());
        o.initialized.insert(n.into());
        o.instances.insert(n.into(), "Running".into());
    }
    assert!(plan(&target, &ns, "ubuntu:24.04", &install, &DaemonSettings::default(), &o).unwrap().is_empty());
}

#[test]
fn snap_files_and_debs_are_pushed_before_installing() {
    let target = LxdTarget { remote: Some("prod".into()), project: Some("infra".into()) };
    let steps = plan(
        &target,
        &nodes(&["n1"]),
        "ubuntu:24.04",
        &Install::SnapFile("/tmp/c.snap".into()),
        &DaemonSettings::default(),
        &Observed::default(),
    )
    .unwrap();
    let push = steps.iter().find(|s| s.kind == StepKind::Push).unwrap();
    assert_eq!(push.args, ["file", "push", "/tmp/c.snap", "prod:n1/root/collocate.snap", "--project", "infra"]);
    let install = steps.iter().find(|s| s.kind == StepKind::Install).unwrap();
    assert_eq!(install.args, ["exec", "prod:n1", "--project", "infra", "--", "snap", "install", "--dangerous", "/root/collocate.snap"]);
    let profile = steps.iter().find(|s| s.kind == StepKind::Profile).unwrap();
    assert_eq!(profile.args, ["profile", "create", &format!("prod:{PROFILE}"), "--project", "infra"]);

    let debs = plan(
        &LxdTarget::default(),
        &nodes(&["n1"]),
        "ubuntu:24.04",
        &Install::Deb(vec!["/x/collocated_0.1.0-1_amd64.deb".into(), "/x/collocate_0.1.0-1_amd64.deb".into()]),
        &DaemonSettings::default(),
        &Observed::default(),
    )
    .unwrap();
    let k = kinds_for(&debs, "n1");
    assert_eq!(k.iter().filter(|x| **x == StepKind::Push).count(), 2);
    assert!(k.contains(&StepKind::Enable));
    assert!(!k.contains(&StepKind::Connect));
    let apt = debs.iter().rfind(|s| s.kind == StepKind::Install).unwrap();
    assert!(apt.args.ends_with(&["/root/collocated_0.1.0-1_amd64.deb".to_string(), "/root/collocate_0.1.0-1_amd64.deb".to_string()]));
}

#[test]
fn launch_arguments_use_the_profile_limits_and_target() {
    let rec = NodeRecord { target: Some("lxd2".into()), image: None, cpus: Some(1.5), memory: Some("4g".into()) };
    let a = launch_args(&LxdTarget::default(), "n1", &rec, "ubuntu:24.04");
    assert_eq!(a[..7], ["launch", "ubuntu:24.04", "n1", "--profile", "default", "--profile", PROFILE]);
    for pair in [["--target", "lxd2"], ["-c", "limits.cpu=2"], ["-c", "limits.memory=4GiB"]] {
        assert!(a.windows(2).any(|w| w == pair), "{pair:?} missing in {a:?}");
    }
}

#[test]
fn lxc_json_is_parsed() {
    let states = instance_states(r#"[{"name":"n1","status":"Running"},{"name":"n2","status":"Stopped"}]"#).unwrap();
    assert_eq!(states["n1"], "Running");
    assert_eq!(states["n2"], "Stopped");
    assert!(instance_states("{}").is_err());
    assert_eq!(cluster_members(r#"[{"server_name":"a"},{"server_name":"b"}]"#), vec!["a", "b"]);
    assert!(cluster_members("nope").is_empty());
    assert!(reports_initialized(r#"{"initialized":true}"#));
    assert!(!reports_initialized(r#"{"initialized":false}"#));
    assert!(!reports_initialized("garbage"));
}

#[test]
fn node_preseeds_name_the_node() {
    let s = DaemonSettings { subnet: "10.50.0.0/16".into(), ..DaemonSettings::default() };
    let p = Preseed::parse(&node_preseed(&s, "edge-1").unwrap()).unwrap();
    assert_eq!(p.daemon.subnet, "10.50.0.0/16");
    assert_eq!(p.daemon.node_name.as_deref(), Some("edge-1"));
}

#[test]
fn preseeds_parse_validate_and_hide_tokens() {
    let yaml = "mode: lxd\ndaemon:\n  subnet: 10.1.0.0/16\n  defaults:\n    memory: 512m\ncluster:\n  remote: prod\n  url: https://10.0.0.1:8443\n  token: secret\n  install: channel:latest/edge\n  nodes:\n    n1: {target: lxd1}\n    n2: {}\n";
    let p = Preseed::parse(yaml).unwrap();
    assert_eq!(p.mode, Mode::Lxd);
    let c = p.cluster.as_ref().unwrap();
    assert_eq!(c.nodes.len(), 2);
    assert_eq!(c.token.as_deref(), Some("secret"));
    assert_eq!(c.image, "ubuntu:24.04");
    let rendered = p.to_yaml().unwrap();
    assert!(!rendered.contains("secret"), "{rendered}");
    let back = Preseed::parse(&rendered).unwrap();
    assert_eq!(back.cluster.unwrap().nodes, c.nodes);

    assert!(Preseed::parse("mode: lxd\n").is_err());
    assert!(Preseed::parse("mode: lxd\ncluster:\n  nodes: {}\n").is_err());
    assert!(Preseed::parse("mode: lxd\ncluster:\n  nodes:\n    9bad: {}\n").is_err());
    assert!(Preseed::parse("mode: lxd\ncluster:\n  install: nope\n  nodes:\n    n1: {}\n").is_err());
    assert!(Preseed::parse("mode: lxd\ncluster:\n  url: https://x\n  nodes:\n    n1: {}\n").is_err());
    assert!(Preseed::parse("bogus: 1\n").is_err());
    assert_eq!(Preseed::parse("").unwrap(), Preseed::default());
}

#[test]
fn registries_round_trip_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("cluster.yaml");
    assert!(Registry::load(&path).unwrap().is_none());
    let mut c = ClusterPreseed { remote: Some("prod".into()), install: "deb:/a.deb".into(), ..ClusterPreseed::default() };
    c.nodes.insert("n1".into(), NodeRecord { target: Some("lxd1".into()), ..NodeRecord::default() });
    let settings = DaemonSettings { subnet: "10.60.0.0/16".into(), node_name: Some("ignored".into()), ..DaemonSettings::default() };
    let r = Registry::from_preseed(&c, &settings);
    assert_eq!(r.daemon.subnet, "10.60.0.0/16");
    assert_eq!(r.daemon.node_name, None);
    r.save(&path).unwrap();
    let back = Registry::load(&path).unwrap().unwrap();
    assert_eq!(back, r);
    assert_eq!(back.relay(), "collocate-relay");
    assert_eq!(back.target().instance("n1"), "prod:n1");
}
