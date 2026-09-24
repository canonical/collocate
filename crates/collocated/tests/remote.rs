use collocate_core::auth::{Caller, Role};
use collocate_core::request::{ContainerInfo, Request, Response, State};
use collocate_core::Error;
use collocated::remote::{authorize, filter_response, host_addresses, token_addresses};

fn caller(role: Role, projects: &[&str]) -> Caller {
    Caller { name: "ci".into(), fingerprint: "ab".repeat(32), role, projects: projects.iter().map(|p| p.to_string()).collect() }
}

fn check(c: &Caller, req: &Request, project_of_target: Option<&str>) -> collocate_core::Result<bool> {
    let owned = project_of_target.map(String::from);
    authorize(c, &req.verb(), &req.access(), &mut |_| Ok(owned.clone()))
}

fn stop(t: &str) -> Request {
    Request::Stop { target: t.into(), timeout_secs: None }
}

#[test]
fn roles_gate_verbs() {
    let viewer = caller(Role::Viewer, &[]);
    assert!(check(&viewer, &Request::Ps { all: true, project: None }, None).is_ok());
    assert!(check(
        &viewer,
        &Request::Logs { target: "w".into(), tail: None, offset: None, source: Default::default(), services: vec![] },
        None
    )
    .is_ok());
    for denied in [
        stop("w"),
        Request::SecretReveal { project: "p".into(), name: "s".into() },
        Request::ImagePull { reference: "busybox".into(), policy: "missing".into(), credentials: vec![] },
        Request::TrustList,
    ] {
        let e = check(&viewer, &denied, None).unwrap_err();
        assert!(matches!(e, Error::Forbidden(_)), "{denied:?}: {e}");
    }
    let operator = caller(Role::Operator, &[]);
    assert!(check(&operator, &stop("w"), None).is_ok());
    assert!(check(&operator, &Request::ImagePrune, None).is_ok());
    assert!(matches!(check(&operator, &Request::TrustTokenList, None), Err(Error::Forbidden(_))));
    assert!(matches!(check(&operator, &Request::Shutdown, None), Err(Error::Forbidden(_))));
    let admin = caller(Role::Admin, &[]);
    assert!(check(&admin, &Request::TrustTokenList, None).is_ok());
    assert!(check(&admin, &Request::Init { settings: Default::default(), force: false }, None).is_ok());
    for internal in [
        Request::TrustLookup { fingerprint: "x".into() },
        Request::TrustEnroll { secret: "s".into(), certificate: "c".into(), name: None },
        Request::As { caller: admin.clone(), request: Box::new(Request::Info) },
    ] {
        assert!(matches!(check(&admin, &internal, None), Err(Error::Forbidden(_))), "{internal:?}");
    }
}

#[test]
fn project_restrictions_follow_targets_and_filter_lists() {
    let web = caller(Role::Operator, &["web"]);
    assert!(check(&web, &stop("w"), Some("web")).is_ok());
    assert!(matches!(check(&web, &stop("w"), Some("db")), Err(Error::Forbidden(_))));
    assert!(matches!(check(&web, &stop("w"), None), Err(Error::Forbidden(_))));
    assert!(check(&web, &Request::SecretSet { project: "web".into(), name: "a".into(), value: "b".into() }, None).is_ok());
    assert!(check(&web, &Request::SecretSet { project: "db".into(), name: "a".into(), value: "b".into() }, None).is_err());
    assert!(check(&web, &Request::ImagePrune, None).is_err());
    assert!(check(&web, &Request::ImagePull { reference: "busybox".into(), policy: "missing".into(), credentials: vec![] }, None).is_ok());
    assert!(check(&web, &Request::Ps { all: true, project: None }, None).unwrap());
    assert!(!check(&web, &Request::Ps { all: true, project: Some("web".into()) }, None).unwrap());
    assert!(check(&web, &Request::Ps { all: true, project: Some("db".into()) }, None).is_err());
    let info = |name: &str, project: Option<&str>| ContainerInfo {
        id: collocate_core::ContainerId::parse(&format!("{:0>12}", name.len())).unwrap(),
        name: name.into(),
        state: State::Running,
        pid: None,
        address: None,
        project: project.map(String::from),
        service: None,
        health: None,
        revision: None,
        series: None,
        published: vec![],
        image_kind: Default::default(),
    };
    let filtered = filter_response(Response::Containers(vec![info("a", Some("web")), info("bb", Some("db")), info("ccc", None)]), &web);
    assert!(matches!(&filtered, Response::Containers(c) if c.len() == 1 && c[0].name == "a"), "{filtered:?}");
    let names = filter_response(Response::Names(vec!["web/x".into(), "db/y".into()]), &web);
    assert_eq!(names, Response::Names(vec!["web/x".into()]));
}

#[test]
fn token_addresses_expand_wildcards() {
    let json = r#"[{"ifname":"lo","addr_info":[{"local":"127.0.0.1","scope":"host"}]},
        {"ifname":"eth0","addr_info":[{"local":"10.0.10.8","scope":"global"},{"local":"fe80::1","scope":"link"},{"local":"2001:db8::8","scope":"global"}]},
        {"ifname":"collocate0","addr_info":[{"local":"172.30.0.1","scope":"global"}]}]"#;
    let host = host_addresses(json, &["collocate0"]);
    assert_eq!(host.len(), 2);
    assert_eq!(token_addresses(":8443", &host).unwrap(), vec!["10.0.10.8:8443".to_string(), "[2001:db8::8]:8443".to_string()]);
    assert_eq!(token_addresses("0.0.0.0:9000", &host).unwrap(), vec!["10.0.10.8:9000".to_string()]);
    assert_eq!(token_addresses("192.168.1.5:8443", &host).unwrap(), vec!["192.168.1.5:8443".to_string()]);
    assert!(token_addresses(":8443", &[]).is_err());
    assert!(token_addresses("nonsense", &host).is_err());
}
