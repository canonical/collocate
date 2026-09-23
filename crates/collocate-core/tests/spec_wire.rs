use collocate_core::net::Publish;
use collocate_core::request::{ContainerInfo, HealthState, Request, Response, State};
use collocate_core::spec::{Mount, RootSource, Series, Spec};
use collocate_core::wire::{read_frame, write_frame, MAX_FRAME};
use collocate_core::{ContainerId, Error};
use std::io::Cursor;

fn base() -> Spec {
    Spec::new(
        "web1",
        RootSource::Base { series: Series::Noble, build_id: "20260101-abc".into() },
        vec!["/bin/sh".into(), "-c".into(), "true".into()],
    )
}

#[test]
fn series_parses_versions_and_codenames() {
    assert_eq!(Series::parse("24.04").unwrap(), Series::Noble);
    assert_eq!(Series::parse("noble").unwrap(), Series::Noble);
    assert_eq!(Series::parse("26.04").unwrap(), Series::Resolute);
    assert_eq!(Series::parse("resolute").unwrap(), Series::Resolute);
    assert!(Series::parse("22.04").is_err());
    assert_eq!(Series::Noble.dir_name(), "noble");
}

#[test]
fn new_spec_has_safe_defaults() {
    let s = base();
    assert!(!s.persistent);
    assert_eq!(s.hostname, "web1");
    assert_eq!(s.limits.pids_max, 4096);
    assert_eq!(s.process.user, "0");
    assert_eq!(s.process.workdir, "/");
    assert!(s.validate().is_ok());
}

#[test]
fn spec_serde_roundtrips() {
    let mut s = base();
    s.net.publish.push(Publish::parse("8080:80").unwrap());
    s.mounts.push(Mount::Tmpfs { dst: "/tmp".into(), size: Some(64 << 20) });
    let j = serde_json::to_string(&s).unwrap();
    let back: Spec = serde_json::from_str(&j).unwrap();
    assert_eq!(s, back);
}

#[test]
fn spec_tolerates_missing_optional_fields() {
    let s = base();
    let mut v: serde_json::Value = serde_json::to_value(&s).unwrap();
    v.as_object_mut().unwrap().remove("restart");
    v.as_object_mut().unwrap().remove("caps");
    let back: Spec = serde_json::from_value(v).unwrap();
    assert_eq!(back.restart, Default::default());
}

#[test]
fn validation_rejects_bad_names_and_empty_argv() {
    let mut s = base();
    s.name = "Bad Name".into();
    assert!(matches!(s.validate(), Err(Error::InvalidSpec(_))));
    let mut s = base();
    s.process.argv.clear();
    assert!(matches!(s.validate(), Err(Error::InvalidSpec(_))));
}

#[test]
fn validation_rejects_duplicate_host_ports() {
    let mut s = base();
    s.net.publish.push(Publish::parse("8080:80").unwrap());
    s.net.publish.push(Publish::parse("8080:81").unwrap());
    assert!(s.validate().is_err());
    let mut s = base();
    s.net.publish.push(Publish::parse("53:53/tcp").unwrap());
    s.net.publish.push(Publish::parse("53:53/udp").unwrap());
    assert!(s.validate().is_ok());
}

#[test]
fn validation_rejects_duplicate_mount_destinations() {
    let mut s = base();
    s.mounts.push(Mount::Tmpfs { dst: "/tmp".into(), size: None });
    s.mounts.push(Mount::Bind { src: "/a".into(), dst: "/tmp".into(), ro: false });
    assert!(s.validate().is_err());
}

#[test]
fn validation_rejects_persistent_with_no_name() {
    let mut s = base();
    s.persistent = true;
    s.name.clear();
    assert!(s.validate().is_err());
}

#[test]
fn spec_hash_ignores_identity_and_runtime_fields() {
    let a = base();
    let mut b = base();
    b.id = ContainerId::from_bytes([1, 2, 3, 4, 5, 6]);
    b.created = 12345;
    b.exit_status = Some(1);
    assert_eq!(a.spec_hash(), b.spec_hash());
}

#[test]
fn spec_hash_changes_with_configuration() {
    let a = base();
    let mut b = base();
    b.process.env.push(("A".into(), "1".into()));
    assert_ne!(a.spec_hash(), b.spec_hash());
}

#[test]
fn frames_roundtrip() {
    let req = Request::Ps { all: true, project: Some("myapp".into()) };
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).unwrap();
    assert_eq!(&buf[..4], &((buf.len() - 4) as u32).to_be_bytes());
    let back: Request = read_frame(&mut Cursor::new(buf)).unwrap();
    assert_eq!(back, req);
}

#[test]
fn oversize_frames_are_rejected_before_allocation() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
    let r: Result<Request, _> = read_frame(&mut Cursor::new(buf));
    assert!(matches!(r, Err(Error::FrameTooLarge(_))));
}

#[test]
fn truncated_frames_error() {
    let mut buf = Vec::new();
    write_frame(&mut buf, &Request::Info).unwrap();
    buf.truncate(buf.len() - 1);
    let r: Result<Request, _> = read_frame(&mut Cursor::new(buf));
    assert!(r.is_err());
}

#[test]
fn clean_eof_is_distinguishable() {
    let r: Result<Request, _> = read_frame(&mut Cursor::new(Vec::new()));
    assert!(matches!(r, Err(Error::Eof)));
}

#[test]
fn responses_roundtrip_including_errors() {
    let info = ContainerInfo {
        id: ContainerId::from_bytes([1; 6]),
        name: "db".into(),
        state: State::Running,
        pid: Some(4211),
        address: Some("172.30.0.2".parse().unwrap()),
        project: Some("myapp".into()),
        service: Some("db".into()),
        health: Some(HealthState::Healthy),
        revision: Some("abc123".into()),
        series: Some("24.04".into()),
        published: vec!["8080:80/tcp".into()],
        image_kind: None,
    };
    for r in [Response::Containers(vec![info]), Response::error(&Error::NotFound("x".into()))] {
        let mut buf = Vec::new();
        write_frame(&mut buf, &r).unwrap();
        let back: Response = read_frame(&mut Cursor::new(buf)).unwrap();
        assert_eq!(back, r);
    }
}

#[test]
fn error_exit_codes_follow_the_cli_contract() {
    assert_eq!(Error::NotFound("x".into()).exit_code(), 3);
    assert_eq!(Error::Conflict("x".into()).exit_code(), 5);
    assert_eq!(Error::Timeout("x".into()).exit_code(), 6);
    assert_eq!(Error::InvalidSpec("x".into()).exit_code(), 2);
}
