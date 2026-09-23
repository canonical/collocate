use collocate_core::net::{nft_tag, veth_names};
use collocate_core::spec::Spec;
use collocate_core::ContainerId;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct Observed {
    pub cgroups: HashMap<ContainerId, bool>,
    pub veths: Vec<String>,
    pub nft_tags: Vec<String>,
    pub leftovers: Vec<ContainerId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Adopt(ContainerId),
    MarkStopped(ContainerId),
    RemoveSpec(ContainerId),
    CleanRuntime(ContainerId),
    KillCgroup(ContainerId),
    RemoveCgroup(ContainerId),
    DeleteVeth(String),
    DeleteNftTag(String),
}

pub fn plan(specs: &[Spec], observed: &Observed) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut adopted: HashSet<ContainerId> = HashSet::new();
    let mut cleaned: HashSet<ContainerId> = HashSet::new();
    let known: HashSet<ContainerId> = specs.iter().map(|s| s.id).collect();

    for spec in specs {
        match observed.cgroups.get(&spec.id) {
            Some(true) => {
                adopted.insert(spec.id);
                actions.push(Action::Adopt(spec.id));
                continue;
            }
            Some(false) => actions.push(Action::RemoveCgroup(spec.id)),
            None => {}
        }
        if spec.persistent {
            actions.push(Action::MarkStopped(spec.id));
        } else {
            actions.push(Action::RemoveSpec(spec.id));
        }
        actions.push(Action::CleanRuntime(spec.id));
        cleaned.insert(spec.id);
    }

    let unknown: BTreeSet<ContainerId> = observed.cgroups.keys().copied().filter(|id| !known.contains(id)).collect();
    for id in unknown {
        actions.push(Action::KillCgroup(id));
        actions.push(Action::RemoveCgroup(id));
    }

    let kept_veths: HashSet<String> = adopted.iter().map(|id| veth_names(id).0).collect();
    let mut veths = observed.veths.clone();
    veths.sort();
    for v in veths {
        if v.starts_with("vh") && v.len() == 12 && !kept_veths.contains(&v) {
            actions.push(Action::DeleteVeth(v));
        }
    }

    let kept_tags: HashSet<String> = adopted.iter().map(nft_tag).collect();
    let mut tags = observed.nft_tags.clone();
    tags.sort();
    for t in tags {
        if t.starts_with("collocate:") && !kept_tags.contains(&t) {
            actions.push(Action::DeleteNftTag(t));
        }
    }

    let mut leftovers = observed.leftovers.clone();
    leftovers.sort();
    for id in leftovers {
        if !adopted.contains(&id) && cleaned.insert(id) {
            actions.push(Action::CleanRuntime(id));
        }
    }
    actions
}
