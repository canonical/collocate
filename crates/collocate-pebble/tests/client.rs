use collocate_pebble::time::rfc3339_nanos;
use collocate_pebble::{Client, Error};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
    chunked: bool,
}

fn json(status: u16, body: &str) -> Reply {
    Reply { status, content_type: "application/json", body: body.to_string(), chunked: false }
}

struct FakePebble {
    socket: PathBuf,
    seen: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

fn serve(routes: HashMap<&'static str, Reply>) -> FakePebble {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join(".pebble.socket");
    let listener = UnixListener::bind(&socket).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            r.read_line(&mut request_line).unwrap();
            loop {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                if l == "\r\n" || l.is_empty() {
                    break;
                }
            }
            let path = request_line.split_whitespace().nth(1).unwrap().to_string();
            log.lock().unwrap().push(path.clone());
            let route = path.split('?').next().unwrap().to_string();
            let missing = json(404, r#"{"type":"error","status-code":404,"result":{"message":"not found"}}"#);
            let reply = routes.get(route.as_str()).unwrap_or(&missing);
            if reply.chunked {
                write!(stream, "HTTP/1.1 {} X\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\n\r\n", reply.status, reply.content_type).unwrap();
                for piece in reply.body.as_bytes().chunks(7) {
                    write!(stream, "{:x}\r\n", piece.len()).unwrap();
                    stream.write_all(piece).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                }
                stream.write_all(b"0\r\n\r\n").unwrap();
            } else {
                write!(stream, "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n{}", reply.status, reply.content_type, reply.body.len(), reply.body).unwrap();
            }
        }
    });
    FakePebble { socket, seen, _dir: dir }
}

fn client(p: &FakePebble) -> Client {
    Client::new(&p.socket, Duration::from_secs(2))
}

#[test]
fn healthy_when_checks_up_and_services_active() {
    let mut routes = HashMap::new();
    routes.insert("/v1/checks", json(200, r#"{"type":"sync","status-code":200,"status":"OK","result":[{"name":"db","level":"ready","status":"up","failures":0,"threshold":3},{"name":"old","status":"inactive"}]}"#));
    routes.insert("/v1/services", json(200, r#"{"type":"sync","status-code":200,"status":"OK","result":[{"name":"postgres","startup":"enabled","current":"active"}]}"#));
    let p = serve(routes);
    let h = client(&p).health(Some("ready")).unwrap();
    assert!(h.healthy, "{:?}", h.problems);
    assert!(p.seen.lock().unwrap().contains(&"/v1/checks?level=ready".to_string()));
}

#[test]
fn down_checks_and_backoff_services_are_reported() {
    let mut routes = HashMap::new();
    routes.insert("/v1/checks", json(200, r#"{"type":"sync","status-code":200,"result":[{"name":"db","status":"down","failures":3,"threshold":3}]}"#));
    routes.insert("/v1/services", json(200, r#"{"type":"sync","status-code":200,"result":[{"name":"postgres","startup":"enabled","current":"backoff"}]}"#));
    let p = serve(routes);
    let h = client(&p).health(None).unwrap();
    assert!(!h.healthy);
    assert_eq!(h.problems, vec!["check db is down (3/3 failures)", "service postgres is in backoff"]);
}

#[test]
fn null_results_mean_nothing_is_configured() {
    let mut routes = HashMap::new();
    routes.insert("/v1/checks", json(200, r#"{"type":"sync","status-code":200,"result":null}"#));
    routes.insert("/v1/services", json(200, r#"{"type":"sync","status-code":200,"result":[]}"#));
    let p = serve(routes);
    assert!(client(&p).health(None).unwrap().healthy);
}

#[test]
fn api_errors_surface_the_pebble_message() {
    let mut routes = HashMap::new();
    routes.insert("/v1/checks", json(400, r#"{"type":"error","status-code":400,"status":"Bad Request","result":{"message":"level must be alive or ready"}}"#));
    let p = serve(routes);
    match client(&p).checks(Some("bogus")) {
        Err(Error::Api { status, message }) => {
            assert_eq!(status, 400);
            assert_eq!(message, "level must be alive or ready");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn chunked_ndjson_logs_are_parsed() {
    let body = concat!(
        r#"{"time":"2024-02-23T16:54:07.1Z","service":"pg","message":"starting\n"}"#,
        "\n",
        r#"{"time":"2024-02-23T16:54:08Z","service":"pg","message":"ready"}"#,
        "\n"
    );
    let mut routes = HashMap::new();
    routes.insert("/v1/logs", Reply { status: 200, content_type: "application/x-ndjson", body: body.into(), chunked: true });
    let p = serve(routes);
    let logs = client(&p).logs(&["pg".to_string()], Some(10)).unwrap();
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].render(), "2024-02-23T16:54:07.1Z [pg] starting\n");
    assert_eq!(logs[1].render(), "2024-02-23T16:54:08Z [pg] ready\n");
    assert!(logs[0].nanos() < logs[1].nanos());
    assert_eq!(p.seen.lock().unwrap()[0], "/v1/logs?n=10&services=pg");
}

#[test]
fn all_logs_are_requested_with_negative_n() {
    let mut routes = HashMap::new();
    routes.insert("/v1/logs", Reply { status: 200, content_type: "application/x-ndjson", body: String::new(), chunked: false });
    let p = serve(routes);
    assert!(client(&p).logs(&[], None).unwrap().is_empty());
    assert_eq!(p.seen.lock().unwrap()[0], "/v1/logs?n=-1");
}

#[test]
fn missing_socket_is_an_io_error() {
    let c = Client::new("/nonexistent/.pebble.socket", Duration::from_millis(100));
    assert!(matches!(c.checks(None), Err(Error::Io(_))));
}

#[test]
fn rfc3339_parsing_handles_fractions_and_offsets() {
    assert_eq!(rfc3339_nanos("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(rfc3339_nanos("1970-01-01T00:00:01.5Z"), Some(1_500_000_000));
    assert_eq!(rfc3339_nanos("1970-01-01T01:00:00+01:00"), Some(0));
    assert_eq!(rfc3339_nanos("2024-02-23T16:54:07.149249155Z"), Some(1_708_707_247_149_249_155));
    assert_eq!(rfc3339_nanos("garbage"), None);
}
