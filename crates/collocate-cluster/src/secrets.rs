use crate::plan::node_of;
use collocate_compose::model::{ComposeFile, Service};
use collocate_compose::template::{references, Reference};
use collocate_core::client::Api;
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

fn secrets_used_by(svc: &Service) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = svc.secrets.iter().cloned().collect();
    for text in svc.env.values().chain(&svc.command).chain(&svc.entrypoint) {
        for r in references(text).unwrap_or_default() {
            if let Reference::Secret(name) = r {
                out.insert(name);
            }
        }
    }
    out
}

pub fn cross_node_secrets(file: &ComposeFile) -> BTreeMap<String, BTreeSet<String>> {
    let mut nodes_by_secret: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (svc_name, svc) in &file.services {
        let Some(node) = node_of(file, svc_name) else { continue };
        for secret in secrets_used_by(svc) {
            nodes_by_secret.entry(secret).or_default().insert(node.clone());
        }
    }
    nodes_by_secret.retain(|name, nodes| nodes.len() > 1 && !file.secrets.get(name).is_some_and(|d| d.per_node));
    nodes_by_secret
}

pub fn replicate_secrets(file: &ComposeFile, node_apis: &mut BTreeMap<String, Box<dyn Api>>) -> Result<()> {
    for (name, nodes) in cross_node_secrets(file) {
        let Some(def) = file.secrets.get(&name) else { continue };
        let Some(owner) = nodes.iter().next().cloned() else { continue };
        let api = node_apis.get_mut(&owner).ok_or_else(|| Error::Internal(format!("no connection to node {owner}")))?;
        if let Some(gen) = &def.generate {
            api.call(Request::SecretEnsure {
                project: file.project.clone(),
                name: name.clone(),
                generate: gen.clone(),
                length: def.length.unwrap_or(24),
            })?;
        }
        let value = match api.call(Request::SecretReveal { project: file.project.clone(), name: name.clone() })? {
            Response::Text { text } => text,
            other => return Err(Error::Internal(format!("unexpected response {other:?}"))),
        };
        for node in nodes.iter().filter(|n| **n != owner) {
            let api = node_apis.get_mut(node).ok_or_else(|| Error::Internal(format!("no connection to node {node}")))?;
            let current = match api.call(Request::SecretReveal { project: file.project.clone(), name: name.clone() }) {
                Ok(Response::Text { text }) => Some(text),
                Ok(_) | Err(Error::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            if current.as_deref() != Some(value.as_str()) {
                api.call(Request::SecretSet { project: file.project.clone(), name: name.clone(), value: value.clone() })?;
            }
        }
    }
    Ok(())
}
