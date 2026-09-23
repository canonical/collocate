use collocate_core::client::{into_result, Client};
use collocate_core::net::{Algorithm, NoBackends, Proto};
use collocate_core::request::{ContainerStats, LbSpec, LbStatus, Request, Response};
use collocate_core::wire::{read_frame, write_frame};
use collocate_core::{ContainerId, Error};
use std::os::unix::net::UnixStream;

fn lb() -> LbSpec {
    LbSpec {
        project: "app".into(),
        name: "web".into(),
        proto: Proto::Tcp,
        listen: 80,
        publish: vec![8080],
        backend_service: "server".into(),
        backend_port: 8080,
        algorithm: Algorithm::RoundRobin,
        on_no_backends: NoBackends::Reject,
        drain_secs: 30,
        vip: Some("172.30.255.1".parse().unwrap()),
    }
}

#[test]
fn new_verbs_roundtrip() {
    let reqs = vec![
        Request::Stats { project: Some("app".into()) },
        Request::LbSet { lb: lb() },
        Request::LbRemove { project: "app".into(), name: "web".into() },
        Request::LbList,
        Request::Shutdown,
        Request::Logs { target: "web".into(), tail: Some(10), offset: Some(5) },
    ];
    for r in reqs {
        let mut buf = Vec::new();
        write_frame(&mut buf, &r).unwrap();
        let back: Request = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(back, r);
    }
}

#[test]
fn new_responses_roundtrip() {
    let stats = ContainerStats {
        id: ContainerId::from_bytes([1; 6]),
        name: "n".into(),
        project: Some("app".into()),
        service: Some("server".into()),
        cpu_usage_usec: 1234,
        memory_current: 1 << 20,
        memory_max: Some(1 << 30),
        pids: 3,
        cpu_limit_milli: Some(1000),
    };
    let status = LbStatus { spec: lb(), backends: vec!["172.30.0.4:8080".into()], vip: "172.30.255.1".parse().unwrap() };
    for r in [Response::Stats(vec![stats]), Response::Lbs(vec![status]), Response::Log { data: "hi".into(), next_offset: 2 }] {
        let mut buf = Vec::new();
        write_frame(&mut buf, &r).unwrap();
        let back: Response = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(back, r);
    }
}

#[test]
fn error_responses_map_back_to_typed_errors() {
    assert!(matches!(into_result(Response::error(&Error::NotFound("x".into()))), Err(Error::NotFound(_))));
    assert!(matches!(into_result(Response::error(&Error::Conflict("x".into()))), Err(Error::Conflict(_))));
    assert!(matches!(into_result(Response::error(&Error::Timeout("x".into()))), Err(Error::Timeout(_))));
    assert!(matches!(into_result(Response::error(&Error::InvalidSpec("x".into()))), Err(Error::Invalid(_))));
    assert!(matches!(into_result(Response::error(&Error::Internal("x".into()))), Err(Error::Internal(_))));
    assert!(matches!(into_result(Response::Ok), Ok(Response::Ok)));
}

#[test]
fn error_messages_do_not_double_their_prefix_across_the_wire() {
    for e in [
        Error::NotFound("web2".into()),
        Error::Conflict("web is running; stop it or use force".into()),
        Error::Timeout("web".into()),
        Error::Unreachable("/tmp/x.sock".into()),
        Error::Invalid("bad memory".into()),
    ] {
        let original = e.to_string();
        let reconstructed = into_result(Response::error(&e)).unwrap_err();
        assert_eq!(reconstructed.to_string(), original, "message should survive the wire unchanged, not gain a second prefix");
    }
}

#[test]
fn client_speaks_the_frame_protocol_over_a_socket() {
    let (a, mut b) = UnixStream::pair().unwrap();
    let server = std::thread::spawn(move || {
        let req: Request = read_frame(&mut b).unwrap();
        assert_eq!(req, Request::Info);
        write_frame(&mut b, &Response::Text { text: "pong".into() }).unwrap();
        let req: Request = read_frame(&mut b).unwrap();
        assert!(matches!(req, Request::LbList));
        write_frame(&mut b, &Response::error(&Error::NotFound("nope".into()))).unwrap();
    });
    let mut c = Client::new(a);
    assert_eq!(c.call(&Request::Info).unwrap(), Response::Text { text: "pong".into() });
    assert!(matches!(c.call(&Request::LbList), Err(Error::NotFound(_))));
    server.join().unwrap();
}

#[test]
fn client_reports_a_closed_connection() {
    let (a, b) = UnixStream::pair().unwrap();
    drop(b);
    let mut c = Client::new(a);
    assert!(c.call(&Request::Info).is_err());
}

#[test]
fn connecting_to_a_missing_socket_is_an_unreachable_error() {
    assert!(matches!(Client::connect("/nonexistent/collocate.sock"), Err(Error::Unreachable(_))));
}
