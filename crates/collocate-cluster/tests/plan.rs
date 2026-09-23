use collocate_cluster::lxc::{exec_args, launch_args, list_args, push_args, running_nodes};
use collocate_cluster::plan::{cross_node_references, provision_plan, Step};
use collocate_compose::model::{ComposeFile, NodeDef};

fn node(cpus: Option<f64>, memory: Option<&str>, target: Option<&str>) -> NodeDef {
    NodeDef { image: Some("ubuntu:24.04".into()), cpus, memory: memory.map(String::from), target: target.map(String::from) }
}

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

#[test]
fn launch_arguments_carry_limits_target_and_nesting() {
    let a = launch_args("edge-1", &node(Some(2.0), Some("2g"), Some("host-a")));
    assert_eq!(a[..4], s(&["launch", "ubuntu:24.04", "edge-1", "--target"])[..]);
    assert!(a.contains(&"host-a".to_string()));
    for c in ["limits.cpu=2", "limits.memory=2GiB", "security.nesting=true"] {
        assert!(a.windows(2).any(|w| w[0] == "-c" && w[1] == c), "{c} missing in {a:?}");
    }
    let bare = launch_args("edge-2", &node(None, None, None));
    assert!(!bare.contains(&"--target".to_string()));
    assert!(!bare.iter().any(|x| x.starts_with("limits.")));
}

#[test]
fn memory_sizes_use_lxd_units() {
    let a = launch_args("n", &node(None, Some("512m"), None));
    assert!(a.contains(&"limits.memory=512MiB".to_string()));
    let a = launch_args("n", &node(None, Some("1073741824"), None));
    assert!(a.contains(&"limits.memory=1073741824B".to_string()));
}

#[test]
fn exec_push_and_list_arguments() {
    assert_eq!(exec_args("edge-1", &s(&["collocate", "ps"])), s(&["exec", "edge-1", "--", "collocate", "ps"]));
    assert_eq!(push_args("./c.deb", "edge-1", "/root/c.deb"), s(&["file", "push", "./c.deb", "edge-1/root/c.deb"]));
    assert_eq!(list_args(), s(&["list", "--format", "json"]));
}

#[test]
fn running_nodes_are_read_from_lxc_json() {
    let json = r#"[{"name":"edge-1","status":"Running"},{"name":"edge-2","status":"Stopped"},{"name":"other","status":"Running"}]"#;
    assert_eq!(running_nodes(json).unwrap(), vec!["edge-1".to_string(), "other".to_string()]);
    assert!(running_nodes("not json").is_err());
}

fn file(y: &str) -> ComposeFile {
    ComposeFile::load(y).unwrap()
}

const CLUSTER: &str = "version: 1\nproject: p\nnodes:\n  edge-1:\n    image: ubuntu:24.04\n  edge-2:\n    image: ubuntu:24.04\nservices:\n  db:\n    node: edge-1\n    series: \"24.04\"\n    command: [/bin/db]\n  app:\n    node: edge-2\n    series: \"24.04\"\n    depends_on: [db]\n    command: [/bin/app]\n";

#[test]
fn provisioning_skips_existing_nodes_and_is_idempotent() {
    let f = file(CLUSTER);
    let plan = provision_plan(&f, &[], "./collocate.deb");
    let launches: Vec<&Step> = plan.iter().filter(|st| matches!(st, Step::Launch { .. })).collect();
    assert_eq!(launches.len(), 2);
    assert!(plan.iter().any(|st| matches!(st, Step::Install { node } if node == "edge-1")));
    let again = provision_plan(&f, &["edge-1".to_string(), "edge-2".to_string()], "./collocate.deb");
    assert!(again.iter().all(|st| !matches!(st, Step::Launch { .. } | Step::Push { .. } | Step::Install { .. })), "{again:?}");
    let partial = provision_plan(&f, &["edge-1".to_string()], "./collocate.deb");
    assert_eq!(partial.iter().filter(|st| matches!(st, Step::Launch { node, .. } if node == "edge-2")).count(), 1);
    assert_eq!(partial.iter().filter(|st| matches!(st, Step::Launch { node, .. } if node == "edge-1")).count(), 0);
}

#[test]
fn launch_precedes_push_and_install_for_each_node() {
    let plan = provision_plan(&file(CLUSTER), &[], "./c.deb");
    for n in ["edge-1", "edge-2"] {
        let idx = |f: &dyn Fn(&Step) -> bool| plan.iter().position(f).unwrap();
        let l = idx(&|s| matches!(s, Step::Launch { node, .. } if node == n));
        let p = idx(&|s| matches!(s, Step::Push { node, .. } if node == n));
        let i = idx(&|s| matches!(s, Step::Install { node } if node == n));
        assert!(l < p && p < i);
    }
}

#[test]
fn cross_node_address_references_are_detected() {
    let mut y = CLUSTER.to_string();
    y = y.replace("command: [/bin/app]", "command: [/bin/app]\n    env:\n      DB: ${services.db.address}");
    let refs = cross_node_references(&file(&y));
    assert_eq!(refs.len(), 1);
    assert!(refs[0].contains("app") && refs[0].contains("db"), "{refs:?}");
}

#[test]
fn same_node_references_and_single_host_files_are_fine() {
    let y = CLUSTER
        .replace("node: edge-2", "node: edge-1")
        .replace("command: [/bin/app]", "command: [/bin/app]\n    env:\n      DB: ${services.db.address}");
    assert!(cross_node_references(&file(&y)).is_empty());
    let single = "version: 1\nproject: p\nservices:\n  a:\n    series: \"24.04\"\n    command: [/bin/a]\n";
    assert!(cross_node_references(&file(single)).is_empty());
}
