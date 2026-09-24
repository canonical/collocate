use crate::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    #[default]
    Operator,
    Admin,
}

impl Role {
    pub fn parse(s: &str) -> Result<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Ok(Role::Viewer),
            "operator" => Ok(Role::Operator),
            "admin" => Ok(Role::Admin),
            other => Err(Error::Invalid(format!("unknown role {other:?} (expected viewer, operator or admin)"))),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    pub name: String,
    pub fingerprint: String,
    pub role: Role,
    #[serde(default)]
    pub projects: Vec<String>,
}

impl Caller {
    pub fn restricted(&self) -> bool {
        !self.projects.is_empty()
    }

    pub fn allows_project(&self, project: Option<&str>) -> bool {
        !self.restricted() || project.is_some_and(|p| self.projects.iter().any(|q| q == p))
    }

    pub fn short(&self) -> String {
        format!("{} ({})", self.name, &self.fingerprint[..self.fingerprint.len().min(12)])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Open,
    Project(Option<String>),
    Filtered(Option<String>),
    Target(String),
    Unrestricted,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub role: Role,
    pub scope: Scope,
}

impl Access {
    pub fn new(role: Role, scope: Scope) -> Access {
        Access { role, scope }
    }
}

pub fn valid_projects(projects: &[String]) -> Result<()> {
    for p in projects {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return Err(Error::Invalid(format!("project name {p:?} is not valid")));
        }
    }
    Ok(())
}
