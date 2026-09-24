use clap::Parser;
use collocate_cli::cli::{Cli, Command, InitArgs};
use collocate_cli::init::{
    apply_flags, default_nodes, failed_checks, fill_targets, interactive, merge_registry, missing_plugs, parse_node_flag, ScriptedPrompter,
};
use collocate_cluster::preseed::{ClusterPreseed, Mode, NodeRecord, Preseed, Registry};
use collocate_core::settings::RootModeSetting;

fn init_args(argv: &[&str]) -> InitArgs {
    let mut full = vec!["collocate", "init"];
    full.extend_from_slice(argv);
    match Cli::try_parse_from(full).unwrap().command {
        Command::Init(a) => *a,
        other => panic!("{other:?}"),
    }
}

#[test]
fn flags_override_the_preseed() {
    let base = Preseed::parse("daemon:\n  subnet: 10.1.0.0/16\n  bridge: br1\n").unwrap();
    let p =
        apply_flags(base, &init_args(&["--auto", "--subnet", "10.9.0.0/16", "--root-mode", "fuse-overlay", "--memory", "256m"])).unwrap();
    assert_eq!(p.mode, Mode::Local);
    assert_eq!(p.daemon.subnet, "10.9.0.0/16");
    assert_eq!(p.daemon.bridge, "br1");
    assert_eq!(p.daemon.root_mode, RootModeSetting::FuseOverlay);
    assert_eq!(p.daemon.defaults.memory.as_deref(), Some("256m"));
    assert!(p.cluster.is_none());
    assert!(apply_flags(Preseed::default(), &init_args(&["--root-mode", "zfs"])).is_err());
    assert!(apply_flags(Preseed::default(), &init_args(&["--install", "scp:x"])).is_err());
}

#[test]
fn lxd_flags_build_the_cluster_section() {
    let p = apply_flags(
        Preseed::default(),
        &init_args(&[
            "--auto",
            "--mode",
            "lxd",
            "--node",
            "edge-a:lxd1",
            "--node",
            "edge-b",
            "--node-memory",
            "2g",
            "--install",
            "deb:/x/a.deb",
            "--lxd-remote",
            "prod",
            "--project",
            "infra",
        ]),
    )
    .unwrap();
    assert_eq!(p.mode, Mode::Lxd);
    let c = p.cluster.unwrap();
    assert_eq!(c.remote.as_deref(), Some("prod"));
    assert_eq!(c.project.as_deref(), Some("infra"));
    assert_eq!(c.install, "deb:/x/a.deb");
    assert_eq!(c.nodes["edge-a"].target.as_deref(), Some("lxd1"));
    assert_eq!(c.nodes["edge-b"].target, None);
    assert!(c.nodes.values().all(|n| n.memory.as_deref() == Some("2g")));
    let counted = apply_flags(Preseed::default(), &init_args(&["--auto", "--mode", "lxd", "--nodes", "3"])).unwrap();
    assert_eq!(counted.cluster.unwrap().nodes.keys().cloned().collect::<Vec<_>>(), ["collocate-1", "collocate-2", "collocate-3"]);
    assert!(parse_node_flag("9lives").is_err());
    assert_eq!(parse_node_flag("n1:").unwrap(), ("n1".to_string(), None));
}

#[test]
fn nodes_are_spread_over_cluster_members() {
    let members = vec!["m1".to_string(), "m2".to_string()];
    let n = default_nodes(3, &members, "c");
    assert_eq!(n["c-1"].target.as_deref(), Some("m1"));
    assert_eq!(n["c-2"].target.as_deref(), Some("m2"));
    assert_eq!(n["c-3"].target.as_deref(), Some("m1"));
    assert!(default_nodes(2, &["solo".to_string()], "c").values().all(|r| r.target.is_none()));
    let mut c = ClusterPreseed::default();
    fill_targets(&mut c, &members, None);
    assert_eq!(c.nodes.len(), 2);
    let mut c = ClusterPreseed::default();
    fill_targets(&mut c, &[], Some(4));
    assert_eq!(c.nodes.len(), 4);
    let mut c = ClusterPreseed::default();
    c.nodes.insert("x".into(), NodeRecord { target: Some("m2".into()), ..NodeRecord::default() });
    c.nodes.insert("y".into(), NodeRecord::default());
    fill_targets(&mut c, &members, None);
    assert_eq!(c.nodes["x"].target.as_deref(), Some("m2"));
    assert!(c.nodes["y"].target.is_some());
}

#[test]
fn the_local_interview_uses_defaults_for_empty_answers() {
    let mut p = ScriptedPrompter::new(&["", "10.77.0.0/16", "", "bogus", "bind-ro", "", "1g"]);
    let out = interactive(&mut p, Preseed::default(), &|_| unreachable!()).unwrap();
    assert_eq!(out.mode, Mode::Local);
    assert_eq!(out.daemon.subnet, "10.77.0.0/16");
    assert_eq!(out.daemon.bridge, "collocate0");
    assert_eq!(out.daemon.root_mode, RootModeSetting::BindRo);
    assert_eq!(out.daemon.defaults.memory.as_deref(), Some("1g"));
    assert!(p.asked.iter().filter(|q| q.starts_with("Root filesystem mode")).count() == 2);
}

#[test]
fn the_lxd_interview_asks_about_nodes_and_installation() {
    let mut p = ScriptedPrompter::new(&["lxd", "", "", "3", "edge", "", "2", "", "deb:/tmp/c.deb", "", "", "", "", ""]);
    let members = |_: &ClusterPreseed| Ok(vec!["m1".to_string(), "m2".to_string()]);
    let out = interactive(&mut p, Preseed::default(), &members).unwrap();
    assert_eq!(out.mode, Mode::Lxd);
    let c = out.cluster.unwrap();
    assert_eq!(c.remote, None);
    assert_eq!(c.project, None);
    assert_eq!(c.nodes.keys().cloned().collect::<Vec<_>>(), ["edge-1", "edge-2", "edge-3"]);
    assert_eq!(c.nodes["edge-2"].target.as_deref(), Some("m2"));
    assert!(c.nodes.values().all(|n| n.cpus == Some(2.0) && n.memory.is_none()));
    assert_eq!(c.install, "deb:/tmp/c.deb");
    assert_eq!(c.image, "ubuntu:24.04");
    assert!(p.asked.iter().any(|q| q.starts_with("Container subnet")));
}

#[test]
fn repeated_bad_answers_give_up() {
    let mut p = ScriptedPrompter::new(&["cloud", "space", "moon"]);
    assert!(interactive(&mut p, Preseed::default(), &|_| Ok(vec![])).is_err());
}

#[test]
fn daemon_info_reports_missing_plugs_and_fatal_checks() {
    let info: serde_json::Value = serde_json::from_str(
        r#"{"interfaces":[{"plug":"docker-privileged","connected":false},{"plug":"network-control","connected":true}],
            "checks":[{"name":"overlayfs","ok":false,"detail":"no"},{"name":"cgroup2","ok":false,"detail":"missing"},{"name":"pidfd","ok":true,"detail":""}]}"#,
    )
    .unwrap();
    assert_eq!(missing_plugs(&info), vec!["docker-privileged".to_string()]);
    assert_eq!(failed_checks(&info), vec!["cgroup2: missing".to_string()]);
    assert!(missing_plugs(&serde_json::json!({})).is_empty());
}

#[test]
fn registries_merge_only_for_the_same_lxd_target() {
    let mut a = Registry::default();
    a.nodes.insert("n1".into(), NodeRecord::default());
    let mut b = Registry::default();
    b.nodes.insert("n2".into(), NodeRecord::default());
    b.install = "channel:latest/edge".into();
    let merged = merge_registry(Some(a.clone()), b.clone());
    assert_eq!(merged.nodes.len(), 2);
    assert_eq!(merged.install, "channel:latest/edge");
    let mut other = b.clone();
    other.remote = Some("prod".into());
    assert_eq!(merge_registry(Some(a), other.clone()), other);
    assert_eq!(merge_registry(None, b.clone()), b);
}

#[test]
fn unreadable_packages_are_reported_before_provisioning() {
    use collocate_cli::init::{check_install_files, snapd_hint, unreadable_hint};
    use collocate_cluster::provision::Install;
    let dir = tempfile::tempdir().unwrap();
    let deb = dir.path().join("a.deb");
    std::fs::write(&deb, b"x").unwrap();
    let ok = Install::parse(&format!("deb:{}", deb.display())).unwrap();
    assert_eq!(ok.files(), vec![deb.display().to_string()]);
    check_install_files(&ok, true).unwrap();
    check_install_files(&Install::Channel("latest/stable".into()), true).unwrap();
    let missing = Install::parse(&format!("file:{}", dir.path().join("none.snap").display())).unwrap();
    let e = check_install_files(&missing, true).unwrap_err().to_string();
    assert!(e.contains("none.snap") && !e.contains("home-all"), "{e}");
    let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    assert!(unreadable_hint("/home/u/c.deb", &denied, true).contains("snap connect collocate:home-all"));
    assert!(!unreadable_hint("/home/u/c.deb", &denied, false).contains("home-all"));
    assert!(snapd_hint("edge-1").contains("edge-1") && snapd_hint("edge-1").contains("--install deb:"));
}
