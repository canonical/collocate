use collocate_core::spec::{RootSource, Series, Spec, SPEC_SCHEMA};
use collocate_core::{ContainerId, Error};
use collocate_store::{NodeStore, Runtime};
use std::fs;

fn spec(name: &str, persistent: bool) -> Spec {
    let mut s = Spec::new(name, RootSource::Base { series: Series::Noble, build_id: "b1".into() }, vec!["/bin/true".into()]);
    s.persistent = persistent;
    s
}

fn store() -> (tempfile::TempDir, NodeStore) {
    let dir = tempfile::tempdir().unwrap();
    let st = NodeStore::open(dir.path().join("state"), dir.path().join("run")).unwrap();
    (dir, st)
}

#[test]
fn persistent_specs_live_under_state_and_ephemeral_under_run() {
    let (dir, st) = store();
    let p = spec("p1", true);
    let e = spec("e1", false);
    st.create(&p).unwrap();
    st.create(&e).unwrap();
    assert!(dir.path().join(format!("state/containers/{}/spec.json", p.id)).exists());
    assert!(dir.path().join(format!("run/containers/{}/spec.json", e.id)).exists());
    let all = st.load_all().unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn get_finds_either_kind_and_missing_is_not_found() {
    let (_d, st) = store();
    let p = spec("p1", true);
    st.create(&p).unwrap();
    assert_eq!(st.get(&p.id).unwrap(), p);
    let missing = ContainerId::from_bytes([9; 6]);
    assert!(matches!(st.get(&missing), Err(Error::NotFound(_))));
}

#[test]
fn duplicate_ids_and_names_conflict() {
    let (_d, st) = store();
    let a = spec("web", false);
    st.create(&a).unwrap();
    assert!(matches!(st.create(&a), Err(Error::Conflict(_))));
    let mut b = spec("web", false);
    b.id = ContainerId::from_bytes([1; 6]);
    assert!(matches!(st.create(&b), Err(Error::Conflict(_))));
}

#[test]
fn empty_names_do_not_collide() {
    let (_d, st) = store();
    let mut a = spec("", false);
    let mut b = spec("", false);
    a.id = ContainerId::from_bytes([1; 6]);
    b.id = ContainerId::from_bytes([2; 6]);
    st.create(&a).unwrap();
    st.create(&b).unwrap();
}

#[test]
fn update_replaces_atomically_without_leftovers() {
    let (dir, st) = store();
    let mut s = spec("p1", true);
    st.create(&s).unwrap();
    s.exit_status = Some(3);
    st.update(&s).unwrap();
    assert_eq!(st.get(&s.id).unwrap().exit_status, Some(3));
    let entries: Vec<_> = fs::read_dir(dir.path().join(format!("state/containers/{}", s.id)))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(entries, vec!["spec.json".to_string()]);
}

#[test]
fn update_of_unknown_container_is_not_found() {
    let (_d, st) = store();
    assert!(matches!(st.update(&spec("x", false)), Err(Error::NotFound(_))));
}

#[test]
fn remove_deletes_and_recover_empties_trash() {
    let (dir, st) = store();
    let s = spec("p1", true);
    st.create(&s).unwrap();
    st.remove(&s.id).unwrap();
    assert!(matches!(st.get(&s.id), Err(Error::NotFound(_))));
    st.recover().unwrap();
    let trash = dir.path().join("state/.trash");
    assert!(!trash.exists() || fs::read_dir(trash).unwrap().next().is_none());
}

#[test]
fn half_created_containers_are_invisible_and_recovered() {
    let (dir, st) = store();
    let tmp = dir.path().join("state/containers/.tmp-deadbeef0000");
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("spec.json"), "{}").unwrap();
    assert!(st.load_all().unwrap().is_empty());
    st.recover().unwrap();
    assert!(!tmp.exists());
}

#[test]
fn corrupt_specs_are_quarantined_not_fatal() {
    let (dir, st) = store();
    let good = spec("good", true);
    st.create(&good).unwrap();
    let bad_id = ContainerId::from_bytes([7; 6]);
    let bad_dir = dir.path().join(format!("state/containers/{bad_id}"));
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("spec.json"), "{not json").unwrap();
    let all = st.load_all().unwrap();
    assert_eq!(all.len(), 1);
    assert!(dir.path().join(format!("state/.corrupt/{bad_id}")).exists());
}

#[test]
fn newer_schema_refuses_to_load() {
    let (dir, st) = store();
    let mut s = spec("p1", true);
    s.schema = SPEC_SCHEMA + 1;
    let d = dir.path().join(format!("state/containers/{}", s.id));
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("spec.json"), serde_json::to_vec(&s).unwrap()).unwrap();
    assert!(matches!(st.load_all(), Err(Error::Invalid(_))));
}

#[test]
fn load_all_is_ordered_by_creation_then_id() {
    let (_d, st) = store();
    let mut a = spec("a", false);
    let mut b = spec("b", false);
    a.created = 20;
    b.created = 10;
    st.create(&a).unwrap();
    st.create(&b).unwrap();
    let names: Vec<_> = st.load_all().unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names, vec!["b", "a"]);
}

#[test]
fn runtime_roundtrips_and_clears() {
    let (_d, st) = store();
    let s = spec("p1", true);
    st.create(&s).unwrap();
    assert!(st.read_runtime(&s.id).unwrap().is_none());
    let rt = Runtime {
        pid: 4211,
        starttime: 987654,
        cgroup: "collocate.slice/containers/x".into(),
        address: Some("172.30.0.2".parse().unwrap()),
        health: None,
    };
    st.write_runtime(&s.id, &rt).unwrap();
    assert_eq!(st.read_runtime(&s.id).unwrap(), Some(rt));
    st.clear_runtime(&s.id).unwrap();
    assert!(st.read_runtime(&s.id).unwrap().is_none());
}

#[test]
fn second_lock_is_refused_until_released() {
    let (_d, st) = store();
    let first = st.lock().unwrap();
    assert!(matches!(st.lock(), Err(Error::Conflict(_))));
    drop(first);
    assert!(st.lock().is_ok());
}

#[test]
fn node_identity_is_created_once_and_checked() {
    let (_d, st) = store();
    let a = st.open_node("edge-1", "172.30.0.0/16", "inst-a").unwrap();
    let b = st.open_node("edge-1", "172.30.0.0/16", "inst-a").unwrap();
    assert_eq!(a.node_uuid, b.node_uuid);
    assert!(matches!(st.open_node("edge-1", "172.30.0.0/16", "inst-b"), Err(Error::Conflict(_))));
}

#[test]
fn subnet_changes_are_refused() {
    let (_d, st) = store();
    st.open_node("n", "172.30.0.0/16", "i").unwrap();
    assert!(matches!(st.open_node("n", "10.9.0.0/16", "i"), Err(Error::Conflict(_))));
}

#[test]
fn adopt_keeps_uuid_and_reinit_clears_containers() {
    let (_d, st) = store();
    let a = st.open_node("n", "172.30.0.0/16", "i1").unwrap();
    st.create(&spec("p1", true)).unwrap();
    let adopted = st.adopt_node("i2").unwrap();
    assert_eq!(adopted.node_uuid, a.node_uuid);
    assert!(st.open_node("n", "172.30.0.0/16", "i2").is_ok());
    assert_eq!(st.load_all().unwrap().len(), 1);
    let fresh = st.reinit_node("i3").unwrap();
    assert_ne!(fresh.node_uuid, a.node_uuid);
    assert!(st.load_all().unwrap().is_empty());
}

#[test]
fn node_metadata_follows_init_but_never_strands_containers() {
    let dir = tempfile::tempdir().unwrap();
    let store = collocate_store::NodeStore::open(dir.path().join("state"), dir.path().join("run")).unwrap();
    store.open_node("local", "10.1.0.0/16", "machine").unwrap();
    let renamed = store.update_node("edge-1", "10.1.0.0/16").unwrap();
    assert_eq!(renamed.name, "edge-1");
    let moved = store.update_node("edge-1", "10.2.0.0/16").unwrap();
    assert_eq!(moved.subnet, "10.2.0.0/16");
    assert_eq!(store.open_node("edge-1", "10.2.0.0/16", "machine").unwrap().node_uuid, renamed.node_uuid);
}
