use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

pub struct Route {
    pub content_type: String,
    pub body: Vec<u8>,
}

pub fn route(content_type: &str, body: Vec<u8>) -> Route {
    Route { content_type: content_type.to_string(), body }
}

pub struct FakeRegistry {
    pub addr: String,
}

fn handle(stream: TcpStream, routes: &HashMap<String, Route>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
    }
    let mut stream = stream;
    match routes.get(&path) {
        Some(r) => {
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", r.content_type, r.body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&r.body);
        }
        None => {
            let body = b"not found";
            let head = format!("HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    }
}

impl FakeRegistry {
    pub fn start_bounded(routes: HashMap<String, Route>, max_requests: usize) -> FakeRegistry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        thread::spawn(move || {
            let mut served = 0;
            for stream in listener.incoming().flatten() {
                handle(stream, &routes);
                served += 1;
                if served >= max_requests {
                    break;
                }
            }
        });
        FakeRegistry { addr }
    }
}
