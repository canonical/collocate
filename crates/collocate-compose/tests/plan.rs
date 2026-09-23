use collocate_compose::plan::{diff, shutdown_order, topo_order, Actual, Item};
use std::collections::BTreeMap;

fn deps(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect()
}

#[test]
fn topo_order_puts_dependencies_first_and_is_deterministic() {
    let d = deps(&[("server", &["db", "cache"]), ("db", &[]), ("cache", &[]), ("worker", &["server"])]);
    let order = topo_order(&d).unwrap();
    assert_eq!(order, vec!["cache", "db", "server", "worker"]);
    assert_eq!(order, topo_order(&d).unwrap());
}

#[test]
fn topo_order_reports_cycles() {
    let d = deps(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"]), ("d", &[])]);
    let e = topo_order(&d).unwrap_err().to_string();
    assert!(e.contains("cycle"));
    for n in ["a", "b", "c"] {
        assert!(e.contains(n), "{e}");
    }
    assert!(!e.contains(" d"));
}

#[test]
fn self_dependency_is_a_cycle() {
    assert!(topo_order(&deps(&[("a", &["a"])])).is_err());
}

#[test]
fn unknown_dependencies_are_errors() {
    assert!(topo_order(&deps(&[("a", &["ghost"])])).unwrap_err().to_string().contains("ghost"));
}

#[test]
fn shutdown_is_reverse_of_startup() {
    let order = vec!["db".to_string(), "server".to_string()];
    assert_eq!(shutdown_order(&order), vec!["server", "db"]);
}

fn item(n: &str, h: &str) -> Item {
    Item { name: n.into(), hash: h.into() }
}

fn actual(n: &str, h: &str, running: bool) -> Actual {
    Actual { name: n.into(), hash: h.into(), running }
}

#[test]
fn diff_classifies_every_case() {
    let desired = vec![item("new", "1"), item("same", "2"), item("changed", "3"), item("stopped", "4")];
    let have = vec![actual("same", "2", true), actual("changed", "old", true), actual("stopped", "4", false), actual("extra", "9", true)];
    let p = diff(&desired, &have);
    assert_eq!(p.create, vec!["new"]);
    assert_eq!(p.keep, vec!["same"]);
    assert_eq!(p.recreate, vec!["changed"]);
    assert_eq!(p.start, vec!["stopped"]);
    assert_eq!(p.remove, vec!["extra"]);
}

#[test]
fn diff_of_matching_state_is_a_noop() {
    let p = diff(&[item("a", "1")], &[actual("a", "1", true)]);
    assert!(p.create.is_empty() && p.recreate.is_empty() && p.remove.is_empty() && p.start.is_empty());
    assert!(p.is_noop());
}

#[test]
fn diff_preserves_desired_order() {
    let p = diff(&[item("z", "1"), item("a", "1"), item("m", "1")], &[]);
    assert_eq!(p.create, vec!["z", "a", "m"]);
}
