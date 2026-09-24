use collocate_compose::model::ComposeFile;
use collocate_compose::template::{references, Reference};
use std::collections::BTreeMap;

pub fn node_of(file: &ComposeFile, service: &str) -> Option<String> {
    file.services.get(service)?.node.clone().or_else(|| file.nodes.keys().next().cloned())
}

pub fn cross_node_references(file: &ComposeFile) -> Vec<String> {
    let mut out = Vec::new();
    if file.nodes.len() < 2 {
        return out;
    }
    let placement: BTreeMap<&String, Option<String>> = file.services.keys().map(|s| (s, node_of(file, s))).collect();
    for (name, svc) in &file.services {
        let here = &placement[name];
        for text in svc.env.values().chain(&svc.command).chain(&svc.entrypoint) {
            for r in references(text).unwrap_or_default() {
                if let Reference::ServiceAddress(other) | Reference::ServiceAddresses(other) = r {
                    if placement.get(&other).is_some_and(|there| there != here) {
                        let (a, b) = (here.clone().unwrap_or_default(), placement[&other].clone().unwrap_or_default());
                        out.push(format!("service {name} on {a} references {other} on {b}"));
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
