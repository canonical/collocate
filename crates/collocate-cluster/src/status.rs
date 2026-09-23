use collocate_core::client::Api;
use collocate_core::request::{ContainerInfo, Request, Response};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    pub name: String,
    pub reachable: bool,
    pub containers: Vec<ContainerInfo>,
    pub error: Option<String>,
}

pub fn cluster_status(nodes: &mut [(String, Box<dyn Api>)], project: Option<&str>) -> Vec<NodeStatus> {
    nodes
        .iter_mut()
        .map(|(name, api)| match api.call(Request::Ps { all: true, project: project.map(String::from) }) {
            Ok(Response::Containers(c)) => NodeStatus { name: name.clone(), reachable: true, containers: c, error: None },
            Ok(other) => NodeStatus {
                name: name.clone(),
                reachable: false,
                containers: Vec::new(),
                error: Some(format!("unexpected response {other:?}")),
            },
            Err(e) => NodeStatus { name: name.clone(), reachable: false, containers: Vec::new(), error: Some(e.to_string()) },
        })
        .collect()
}
