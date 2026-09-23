use crate::error::{Error, Result};
use std::collections::BTreeMap;

pub trait Credentials {
    fn for_registry(&self, registry: &str) -> Option<(String, String)>;
}

pub struct Anonymous;

impl Credentials for Anonymous {
    fn for_registry(&self, _registry: &str) -> Option<(String, String)> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BearerChallenge {
    pub realm: String,
    pub service: Option<String>,
    pub scope: Option<String>,
}

fn parse_params(rest: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for part in split_params(rest) {
        if let Some((k, v)) = part.split_once('=') {
            let v = v.trim().trim_matches('"');
            out.insert(k.trim().to_string(), v.to_string());
        }
    }
    out
}

fn split_params(rest: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in rest.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                parts.push(rest[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(rest[start..].trim());
    parts
}

pub fn parse_bearer_challenge(header: &str) -> Result<BearerChallenge> {
    let rest = header.strip_prefix("Bearer ").ok_or_else(|| Error::Auth(format!("unsupported auth scheme: {header}")))?;
    let params = parse_params(rest);
    let realm = params.get("realm").cloned().ok_or_else(|| Error::Auth("bearer challenge missing realm".into()))?;
    Ok(BearerChallenge { realm, service: params.get("service").cloned(), scope: params.get("scope").cloned() })
}
