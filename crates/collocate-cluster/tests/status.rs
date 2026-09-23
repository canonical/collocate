use collocate_cluster::status::{cluster_status, NodeStatus};
use collocate_core::client::Api;
use collocate_core::request::{ContainerInfo, Request, Response, State};
use collocate_core::{ContainerId, Error, Result};

struct Up(Vec<ContainerInfo>);
struct Down;

impl Api for Up {
    fn call(&mut self, req: Request) -> Result<Response> {
        match req {
            Request::Ps { .. } => Ok(Response::Containers(self.0.clone())),
            other => Err(Error::Internal(format!("{other:?}"))),
        }
    }
}

impl Api for Down {
    fn call(&mut self, _: Request) -> Result<Response> {
        Err(Error::Unreachable("no route to node".into()))
    }
}

fn info(name: &str) -> ContainerInfo {
    ContainerInfo {
        id: ContainerId::from_bytes([1; 6]),
        name: name.into(),
        state: State::Running,
        pid: Some(1),
        address: None,
        project: Some("p".into()),
        service: Some("s".into()),
        health: None,
        revision: None,
        series: None,
        published: vec![],
        image_kind: None,
    }
}

#[test]
fn every_node_is_reported_and_unreachable_ones_are_kept() {
    let mut nodes: Vec<(String, Box<dyn Api>)> = vec![
        ("edge-1".into(), Box::new(Up(vec![info("a"), info("b")]))),
        ("edge-2".into(), Box::new(Down)),
        ("edge-3".into(), Box::new(Up(vec![]))),
    ];
    let out = cluster_status(&mut nodes, Some("p"));
    assert_eq!(out.len(), 3);
    assert_eq!(out[0], NodeStatus { name: "edge-1".into(), reachable: true, containers: vec![info("a"), info("b")], error: None });
    assert!(!out[1].reachable && out[1].containers.is_empty());
    assert!(out[1].error.as_deref().unwrap().contains("no route"));
    assert!(out[2].reachable && out[2].containers.is_empty());
}
