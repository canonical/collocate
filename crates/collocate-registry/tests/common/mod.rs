use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

pub struct Route {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub fn route(status: u16, content_type: &str, body: Vec<u8>) -> Route {
    Route { status, headers: vec![("Content-Type".to_string(), content_type.to_string())], body }
}

#[derive(Default)]
pub struct Script {
    pub routes: HashMap<(String, String), Route>,
    pub protected: HashSet<String>,
    pub challenge: String,
}

pub struct FakeServer {
    pub addr: String,
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    }
}

fn write_response(mut stream: &TcpStream, status: u16, headers: &[(String, String)], body: &[u8]) {
    let mut head = format!("HTTP/1.1 {} {}\r\n", status, status_text(status));
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()));
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn handle(stream: TcpStream, script: &Script) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let path_no_query = path.split('?').next().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((k, v)) = line.trim_end().split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    if script.protected.contains(&path_no_query) && !headers.contains_key("authorization") {
        write_response(&stream, 401, &[("WWW-Authenticate".to_string(), script.challenge.clone())], b"unauthorized");
        return;
    }

    match script.routes.get(&(method, path_no_query)) {
        Some(r) => write_response(&stream, r.status, &r.headers, &r.body),
        None => write_response(&stream, 404, &[], b"not found"),
    }
}

impl FakeServer {
    pub fn start(build: impl FnOnce(&str) -> Script) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let script = build(&addr);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(stream, &script);
            }
        });
        FakeServer { addr }
    }
}
