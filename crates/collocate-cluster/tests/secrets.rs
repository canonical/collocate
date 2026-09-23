use collocate_cluster::secrets::{cross_node_secrets, replicate_secrets};
use collocate_compose::model::ComposeFile;
use collocate_core::client::Api;
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use std::collections::BTreeMap;

const STACK: &str = r#"
version: 1
project: shop
nodes:
  edge-1:
    image: ubuntu:24.04
  edge-2:
    image: ubuntu:24.04
services:
  db:
    node: edge-1
    series: "24.04"
    command: ["/bin/db"]
    env:
      PASSWORD: ${secrets.shared}
    secrets: [shared, tls_key]
  server:
    node: edge-2
    series: "24.04"
    command: ["/bin/server"]
    env:
      DB_PASSWORD: ${secrets.shared}
    secrets: [shared, tls_key]
secrets:
  shared:
    generate: password
    length: 16
  tls_key:
    generate: password
    length: 16
    per_node: true
  local_only:
    generate: password
    length: 16
"#;

fn file() -> ComposeFile {
    ComposeFile::load(STACK).unwrap()
}

#[test]
fn a_secret_used_by_services_on_two_nodes_is_flagged() {
    let f = file();
    let cross = cross_node_secrets(&f);
    assert!(cross.contains_key("shared"), "{cross:?}");
    assert_eq!(cross["shared"], ["edge-1".to_string(), "edge-2".to_string()].into_iter().collect());
}

#[test]
fn a_secret_used_on_only_one_node_is_not_flagged() {
    let f = file();
    let cross = cross_node_secrets(&f);
    assert!(!cross.contains_key("local_only"), "{cross:?}");
}

#[test]
fn per_node_secrets_are_never_flagged_even_when_used_on_every_node() {
    let f = file();
    assert!(!cross_node_secrets(&f).contains_key("tls_key"), "{:?}", cross_node_secrets(&f));
}

#[derive(Default)]
struct FakeApi {
    secrets: BTreeMap<String, String>,
    log: Vec<String>,
}

impl Api for FakeApi {
    fn call(&mut self, req: Request) -> Result<Response> {
        match req {
            Request::SecretEnsure { name, .. } => {
                self.secrets.entry(name.clone()).or_insert_with(|| format!("generated-{name}"));
                self.log.push(format!("ensure {name}"));
                Ok(Response::Ok)
            }
            Request::SecretReveal { name, .. } => match self.secrets.get(&name) {
                Some(v) => Ok(Response::Text { text: v.clone() }),
                None => Err(Error::NotFound(name)),
            },
            Request::SecretSet { name, value, .. } => {
                self.secrets.insert(name.clone(), value);
                self.log.push(format!("set {name}"));
                Ok(Response::Ok)
            }
            other => Err(Error::Internal(format!("fake api does not implement {other:?}"))),
        }
    }
}

#[test]
fn replication_generates_once_and_copies_to_every_other_node() {
    let f = file();
    let mut apis: BTreeMap<String, Box<dyn Api>> =
        [("edge-1".to_string(), Box::<FakeApi>::default() as Box<dyn Api>), ("edge-2".to_string(), Box::<FakeApi>::default())]
            .into_iter()
            .collect();
    replicate_secrets(&f, &mut apis).unwrap();

    let reveal = |apis: &mut BTreeMap<String, Box<dyn Api>>, node: &str| match apis
        .get_mut(node)
        .unwrap()
        .call(Request::SecretReveal { project: "shop".into(), name: "shared".into() })
        .unwrap()
    {
        Response::Text { text } => text,
        other => panic!("{other:?}"),
    };
    let a = reveal(&mut apis, "edge-1");
    let b = reveal(&mut apis, "edge-2");
    assert_eq!(a, b, "both nodes should end up with the same secret value");

    let refused = replicate_secrets(&file(), &mut apis);
    assert!(refused.is_ok());
    assert_eq!(reveal(&mut apis, "edge-1"), a, "a second replication should not change an already-consistent secret");
}

#[test]
fn a_node_missing_from_the_api_map_is_a_clear_error() {
    let f = file();
    let mut apis: BTreeMap<String, Box<dyn Api>> =
        [("edge-1".to_string(), Box::<FakeApi>::default() as Box<dyn Api>)].into_iter().collect();
    assert!(replicate_secrets(&f, &mut apis).is_err());
}
