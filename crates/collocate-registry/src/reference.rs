use crate::error::{Error, Result};

const DEFAULT_REGISTRY: &str = "registry-1.docker.io";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Tag(String),
    Digest(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    pub selector: Selector,
}

fn split_registry(s: &str) -> (&str, &str) {
    match s.split_once('/') {
        Some((first, rest)) if first.contains('.') || first.contains(':') || first == "localhost" => (first, rest),
        _ => (DEFAULT_REGISTRY, s),
    }
}

fn split_tag(s: &str) -> (&str, Option<&str>) {
    match s.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') && !tag.is_empty() => (repo, Some(tag)),
        _ => (s, None),
    }
}

impl Reference {
    pub fn parse(s: &str) -> Result<Reference> {
        if s.is_empty() {
            return Err(Error::InvalidReference(s.to_string()));
        }
        let (before_digest, digest) = match s.split_once('@') {
            Some((b, d)) => (b, Some(d.to_string())),
            None => (s, None),
        };
        let (registry, rest) = split_registry(before_digest);
        let (repo, tag) = split_tag(rest);
        let mut repository = repo.to_string();
        if repository.is_empty() {
            return Err(Error::InvalidReference(s.to_string()));
        }
        if registry == DEFAULT_REGISTRY && !repository.contains('/') {
            repository = format!("library/{repository}");
        }
        let selector = match digest {
            Some(d) => Selector::Digest(d),
            None => Selector::Tag(tag.unwrap_or("latest").to_string()),
        };
        Ok(Reference { registry: registry.to_string(), repository, selector })
    }
}
