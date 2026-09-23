use collocate_core::net::{nft_tag, veth_names};
use collocate_core::spec::{RootSource, Series, Spec};
use collocate_core::ContainerId;
use collocate_store::reconcile::{plan, Action, Observed};
use std::collections::HashMap;

fn spec(byte: u8, persistent: bool) -> Spec {
    let mut s = Spec::new(&format!("c{byte}"), RootSource::Base { series: Series::Noble, build_id: "b".into() }, vec!["/bin/true".into()]);
    s.id = ContainerId::from_bytes([byte; 6]);
    s.persistent = persistent;
    s
}

fn empty() -> Observed {
    Observed { cgroups: HashMap::new(), veths: vec![], nft_tags: vec![], leftovers: vec![] }
}

#[test]
fn populated_cgroup_is_adopted_even_without_runtime_file() {
    let s = spec(1, true);
    let mut o = empty();
    o.cgroups.insert(s.id, true);
    let actions = plan(std::slice::from_ref(&s), &o);
    assert!(actions.contains(&Action::Adopt(s.id)));
    assert!(!actions.iter().any(|a| matches!(a, Action::MarkStopped(_) | Action::RemoveSpec(_))));
}

#[test]
fn missing_cgroup_marks_persistent_stopped() {
    let s = spec(2, true);
    let actions = plan(std::slice::from_ref(&s), &empty());
    assert!(actions.contains(&Action::MarkStopped(s.id)));
    assert!(actions.contains(&Action::CleanRuntime(s.id)));
}

#[test]
fn missing_cgroup_removes_ephemeral() {
    let s = spec(3, false);
    let actions = plan(std::slice::from_ref(&s), &empty());
    assert!(actions.contains(&Action::RemoveSpec(s.id)));
}

#[test]
fn empty_cgroup_is_removed() {
    let s = spec(4, true);
    let mut o = empty();
    o.cgroups.insert(s.id, false);
    let actions = plan(std::slice::from_ref(&s), &o);
    assert!(actions.contains(&Action::RemoveCgroup(s.id)));
    assert!(actions.contains(&Action::MarkStopped(s.id)));
}

#[test]
fn unknown_cgroups_are_killed_and_removed() {
    let ghost = ContainerId::from_bytes([9; 6]);
    let mut o = empty();
    o.cgroups.insert(ghost, true);
    let actions = plan(&[], &o);
    assert!(actions.contains(&Action::KillCgroup(ghost)));
    assert!(actions.contains(&Action::RemoveCgroup(ghost)));
}

#[test]
fn orphan_veths_and_tags_are_deleted_but_adopted_ones_kept() {
    let alive = spec(5, true);
    let dead = spec(6, true);
    let mut o = empty();
    o.cgroups.insert(alive.id, true);
    o.veths = vec![veth_names(&alive.id).0, veth_names(&dead.id).0, "eth0".into(), "docker0".into()];
    o.nft_tags = vec![nft_tag(&alive.id), nft_tag(&dead.id), "collocate-lb:app/web".into(), "unrelated".into()];
    let actions = plan(&[alive.clone(), dead.clone()], &o);
    assert!(actions.contains(&Action::DeleteVeth(veth_names(&dead.id).0)));
    assert!(!actions.contains(&Action::DeleteVeth(veth_names(&alive.id).0)));
    assert!(!actions.iter().any(|a| matches!(a, Action::DeleteVeth(n) if n == "eth0" || n == "docker0")));
    assert!(actions.contains(&Action::DeleteNftTag(nft_tag(&dead.id))));
    assert!(!actions.contains(&Action::DeleteNftTag(nft_tag(&alive.id))));
    assert!(!actions.iter().any(|a| matches!(a, Action::DeleteNftTag(t) if t.starts_with("collocate-lb:") || t == "unrelated")));
}

#[test]
fn leftover_dirs_of_non_adopted_containers_are_cleaned() {
    let alive = spec(7, true);
    let gone = ContainerId::from_bytes([8; 6]);
    let mut o = empty();
    o.cgroups.insert(alive.id, true);
    o.leftovers = vec![alive.id, gone];
    let actions = plan(std::slice::from_ref(&alive), &o);
    assert!(actions.contains(&Action::CleanRuntime(gone)));
    assert!(!actions.contains(&Action::CleanRuntime(alive.id)));
}

#[test]
fn plan_is_deterministic_and_ordered_kill_before_remove() {
    let ghost = ContainerId::from_bytes([9; 6]);
    let mut o = empty();
    o.cgroups.insert(ghost, true);
    let actions = plan(&[], &o);
    let k = actions.iter().position(|a| *a == Action::KillCgroup(ghost)).unwrap();
    let r = actions.iter().position(|a| *a == Action::RemoveCgroup(ghost)).unwrap();
    assert!(k < r);
    assert_eq!(actions, plan(&[], &o));
}
