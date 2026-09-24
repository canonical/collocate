use collocate_core::auth::{Caller, Role, Scope};
use collocate_core::request::Request;
use collocate_core::settings::{bind_address, parse_listen_address, DaemonSettings};

#[test]
fn listen_addresses_parse_like_lxd() {
    assert_eq!(parse_listen_address(":8443").unwrap(), (None, 8443));
    assert_eq!(parse_listen_address("10.0.0.5:9000").unwrap(), (Some("10.0.0.5".parse().unwrap()), 9000));
    assert_eq!(parse_listen_address("[::]:8443").unwrap(), (Some("::".parse().unwrap()), 8443));
    assert_eq!(parse_listen_address("10.0.0.5").unwrap(), (Some("10.0.0.5".parse().unwrap()), 8443));
    for bad in ["", ":", ":0", ":99999", "host.example:8443", "10.0.0.5:x"] {
        assert!(parse_listen_address(bad).is_err(), "{bad}");
    }
    assert_eq!(bind_address(":8443").unwrap().to_string(), "0.0.0.0:8443");
    let s = DaemonSettings { https_address: Some("nope".into()), ..DaemonSettings::default() };
    assert!(s.validate().is_err());
    let s = DaemonSettings { https_address: Some(":8443".into()), ..DaemonSettings::default() };
    assert_eq!(DaemonSettings::from_toml(&s.to_toml().unwrap()).unwrap(), s);
}

#[test]
fn every_verb_has_a_role_and_scope() {
    let access = |r: Request| r.access();
    assert_eq!(access(Request::Info).role, Role::Viewer);
    assert_eq!(access(Request::Ps { all: true, project: None }).scope, Scope::Filtered(None));
    assert_eq!(access(Request::Stop { target: "w".into(), timeout_secs: None }).scope, Scope::Target("w".into()));
    assert_eq!(access(Request::SecretReveal { project: "p".into(), name: "s".into() }).role, Role::Operator);
    assert_eq!(access(Request::ImagePrune).scope, Scope::Unrestricted);
    assert_eq!(
        access(Request::TrustTokenCreate { name: "x".into(), role: Role::Viewer, projects: vec![], expiry_secs: None }).role,
        Role::Admin
    );
    assert_eq!(access(Request::TrustLookup { fingerprint: "f".into() }).scope, Scope::Internal);
    assert_eq!(
        access(Request::ConfigPut { project: "p".into(), name: "n".into(), content: "c".into() }).scope,
        Scope::Project(Some("p".into()))
    );
    assert!(Request::ImageImport.needs_descriptors());
    assert!(!Request::Info.needs_descriptors());
    assert_eq!(Request::LbList.verb(), "lb_list");
}

#[test]
fn roles_order_and_callers_check_projects() {
    assert!(Role::Viewer < Role::Operator && Role::Operator < Role::Admin);
    assert_eq!(Role::parse(" Admin ").unwrap(), Role::Admin);
    assert!(Role::parse("root").is_err());
    let open = Caller { name: "a".into(), fingerprint: "f".repeat(64), role: Role::Operator, projects: vec![] };
    assert!(open.allows_project(None) && open.allows_project(Some("x")));
    let web = Caller { projects: vec!["web".into()], ..open.clone() };
    assert!(web.allows_project(Some("web")) && !web.allows_project(Some("db")) && !web.allows_project(None));
    assert_eq!(web.short(), format!("a ({})", "f".repeat(12)));
}
