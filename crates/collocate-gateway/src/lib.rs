use collocate_core::auth::Caller;
use collocate_core::client::{into_result, Client};
use collocate_core::request::{LogSource, Request, Response, State};
use collocate_core::{Error, Result};
use collocate_remote::http::{finish_chunks, read_request, write_chunk, write_chunked_head, write_json, Body, Buffered, Head};
use collocate_remote::tls::{peer_fingerprint, peer_pem};
use collocate_remote::{http_status, CHANNEL_CONTROL, CHANNEL_EXIT, CHANNEL_STDERR, CHANNEL_STDIN, CHANNEL_STDOUT};
use collocate_sys::fdpass::SendWithFds;
use collocate_trust::Token;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::protocol::Role as WsRole;
use tungstenite::{Message, WebSocket};

pub const MAX_CONNECTIONS: usize = 256;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const TRUST_TTL: Duration = Duration::from_secs(5);
const ENROLL_FAILURES: usize = 10;
const ENROLL_WINDOW: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

type Tls = StreamOwned<ServerConnection, TcpStream>;
type Conn = Buffered<Tls>;

macro_rules! log {
    ($($arg:tt)*) => { eprintln!("collocate-gateway: {}", format!($($arg)*)) };
}

pub struct Gateway {
    socket: PathBuf,
    fingerprint: String,
    cache: Mutex<HashMap<String, (Option<Caller>, Instant)>>,
    failures: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    active: AtomicUsize,
}

struct Peer {
    fingerprint: Option<String>,
    pem: Option<String>,
    ip: Option<IpAddr>,
}

fn error_response(e: &Error) -> (u16, Response) {
    (http_status(e.exit_code()), Response::error(e))
}

fn untrusted(peer: &Peer) -> Error {
    match peer.fingerprint {
        None => Error::Forbidden("this endpoint needs a client certificate".into()),
        Some(_) => Error::Forbidden("this client certificate is not trusted; enroll it with a trust token".into()),
    }
}

struct ActiveGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Gateway {
    pub fn new(socket: PathBuf, fingerprint: String) -> Arc<Gateway> {
        Arc::new(Gateway {
            socket,
            fingerprint,
            cache: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            active: AtomicUsize::new(0),
        })
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    fn daemon(&self) -> Result<Client<UnixStream>> {
        Client::connect(&self.socket)
    }

    fn lookup_uncached(&self, fingerprint: &str) -> Option<Caller> {
        let mut c = self.daemon().ok()?;
        match c.call(&Request::TrustLookup { fingerprint: fingerprint.to_string() }) {
            Ok(Response::Json { value }) => serde_json::from_value(value).ok(),
            _ => None,
        }
    }

    pub fn lookup(&self, fingerprint: &str) -> Option<Caller> {
        if let Ok(cache) = self.cache.lock() {
            if let Some((caller, at)) = cache.get(fingerprint) {
                if at.elapsed() < TRUST_TTL {
                    return caller.clone();
                }
            }
        }
        let caller = self.lookup_uncached(fingerprint);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(fingerprint.to_string(), (caller.clone(), Instant::now()));
        }
        caller
    }

    fn forget(&self, fingerprint: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(fingerprint);
        }
    }

    fn throttled(&self, ip: Option<IpAddr>) -> bool {
        let Some(ip) = ip else { return false };
        let Ok(mut f) = self.failures.lock() else { return false };
        let list = f.entry(ip).or_default();
        list.retain(|t| t.elapsed() < ENROLL_WINDOW);
        list.len() >= ENROLL_FAILURES
    }

    fn record_failure(&self, ip: Option<IpAddr>) {
        if let (Some(ip), Ok(mut f)) = (ip, self.failures.lock()) {
            f.entry(ip).or_default().push(Instant::now());
        }
    }

    pub fn try_serve(self: &Arc<Self>, sock: TcpStream, config: Arc<ServerConfig>) -> bool {
        if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        let gw = self.clone();
        std::thread::spawn(move || {
            let _guard = ActiveGuard(&gw.active);
            if let Err(e) = gw.serve(sock, config) {
                if !matches!(e, Error::Eof) {
                    log!("connection ended: {}", e.payload());
                }
            }
        });
        true
    }

    fn serve(self: &Arc<Self>, sock: TcpStream, config: Arc<ServerConfig>) -> Result<()> {
        sock.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        sock.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        sock.set_nodelay(true)?;
        let ip = sock.peer_addr().ok().map(|a| a.ip());
        let conn = ServerConnection::new(config).map_err(|e| Error::Internal(format!("tls: {e}")))?;
        let mut tls = StreamOwned::new(conn, sock);
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock).map_err(|e| Error::Unreachable(format!("tls handshake: {e}")))?;
        }
        let peer = Peer { fingerprint: peer_fingerprint(tls.conn.peer_certificates()), pem: peer_pem(tls.conn.peer_certificates()), ip };
        let mut conn = Buffered::new(tls);
        let mut daemon: Option<Client<UnixStream>> = None;
        loop {
            let head = match read_request(&mut conn) {
                Ok(Some(h)) => h,
                Ok(None) | Err(Error::Eof) => return Ok(()),
                Err(Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(()),
                Err(e) => {
                    let (status, body) = error_response(&Error::Invalid(e.payload()));
                    let _ = write_json(&mut conn, status, &body, false);
                    return Ok(());
                }
            };
            let keep = head.keep_alive();
            let caller = peer.fingerprint.as_deref().and_then(|f| self.lookup(f));
            let route = (head.method.as_str(), head.path.trim_end_matches('/'));
            match route {
                ("GET", "/1.0") => {
                    Body::new(&mut conn, &head, true)?.drain()?;
                    let info = serde_json::json!({
                        "api_version": "1.0",
                        "version": env!("CARGO_PKG_VERSION"),
                        "auth": if caller.is_some() { "trusted" } else { "untrusted" },
                        "caller": caller,
                        "server_fingerprint": self.fingerprint,
                        "client_fingerprint": peer.fingerprint,
                    });
                    write_json(&mut conn, 200, &info, keep)?;
                }
                ("POST", "/1.0/certificates") => {
                    let (status, body) = self.enroll(&mut conn, &head, &peer);
                    write_json(&mut conn, status, &body, keep)?;
                }
                ("POST", "/1.0/call") | ("GET", "/1.0/exec") | ("POST", "/1.0/images") | ("GET", "/1.0/logs") => {
                    let Some(caller) = caller else {
                        let _ = Body::new(&mut conn, &head, true).and_then(|mut b| b.drain());
                        let (status, body) = error_response(&untrusted(&peer));
                        write_json(&mut conn, status, &body, false)?;
                        return Ok(());
                    };
                    match route.1 {
                        "/1.0/call" => {
                            let (status, body) = self.call(&mut conn, &head, caller, &mut daemon);
                            write_json(&mut conn, status, &body, keep)?;
                        }
                        "/1.0/exec" => return self.exec(conn, &head, caller, &peer),
                        "/1.0/images" => return self.images(conn, &head, caller),
                        _ => return self.logs(conn, &head, caller, &peer),
                    }
                }
                (_, "/1.0" | "/1.0/certificates" | "/1.0/call" | "/1.0/exec" | "/1.0/images" | "/1.0/logs") => {
                    let _ = Body::new(&mut conn, &head, true).and_then(|mut b| b.drain());
                    write_json(
                        &mut conn,
                        405,
                        &Response::Error { code: 2, message: format!("{} is not allowed on {}", head.method, head.path) },
                        keep,
                    )?;
                }
                _ => {
                    let _ = Body::new(&mut conn, &head, true).and_then(|mut b| b.drain());
                    write_json(&mut conn, 404, &Response::Error { code: 3, message: format!("no such endpoint {}", head.path) }, keep)?;
                }
            }
            if !keep {
                return Ok(());
            }
        }
    }

    fn enroll(&self, conn: &mut Conn, head: &Head, peer: &Peer) -> (u16, serde_json::Value) {
        let fail = |e: Error| {
            let (s, r) = error_response(&e);
            (s, serde_json::to_value(r).unwrap_or_default())
        };
        let body = match Body::new(conn, head, true).and_then(|mut b| b.read_all(64 * 1024)) {
            Ok(b) => b,
            Err(e) => return fail(e),
        };
        if self.throttled(peer.ip) {
            return (
                429,
                serde_json::to_value(Response::Error { code: 9, message: "too many failed enrollments; try again later".into() })
                    .unwrap_or_default(),
            );
        }
        let result = (|| -> Result<serde_json::Value> {
            let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| Error::Invalid(format!("bad enrollment request: {e}")))?;
            let token = Token::decode(v["token"].as_str().ok_or_else(|| Error::Invalid("the enrollment request needs a token".into()))?)?;
            if !collocate_trust::constant_time_eq(&token.fingerprint, &self.fingerprint) {
                return Err(Error::Invalid("this token was issued for a different server".into()));
            }
            let pem = peer.pem.clone().ok_or_else(|| Error::Invalid("enrollment needs a client certificate".into()))?;
            let name = v["name"].as_str().map(String::from);
            let mut d = self.daemon()?;
            match d.call(&Request::TrustEnroll { secret: token.secret, certificate: pem, name })? {
                Response::Json { mut value } => {
                    if let Some(o) = value.as_object_mut() {
                        o.remove("certificate");
                    }
                    Ok(value)
                }
                other => Err(Error::Internal(format!("unexpected response {other:?}"))),
            }
        })();
        match result {
            Ok(v) => {
                if let Some(fp) = &peer.fingerprint {
                    self.forget(fp);
                }
                log!("enrolled {} from {}", v["name"].as_str().unwrap_or("?"), peer.ip.map(|i| i.to_string()).unwrap_or_default());
                (201, v)
            }
            Err(e) => {
                if matches!(e, Error::Forbidden(_) | Error::Invalid(_)) {
                    self.record_failure(peer.ip);
                }
                fail(e)
            }
        }
    }

    fn daemon_call(&self, daemon: &mut Option<Client<UnixStream>>, req: &Request) -> Result<Response> {
        for attempt in 0..2 {
            if daemon.is_none() {
                *daemon = Some(self.daemon()?);
            }
            let c = daemon.as_mut().expect("daemon connection");
            match c.call(req) {
                Err(Error::Io(_)) | Err(Error::Eof) if attempt == 0 => *daemon = None,
                other => return other,
            }
        }
        Err(Error::Unreachable("the collocate daemon is not answering".into()))
    }

    fn call(&self, conn: &mut Conn, head: &Head, caller: Caller, daemon: &mut Option<Client<UnixStream>>) -> (u16, Response) {
        let result = (|| -> Result<Response> {
            let body = Body::new(conn, head, true)?.read_all(collocate_core::wire::MAX_FRAME)?;
            let req: Request = serde_json::from_slice(&body).map_err(|e| Error::Invalid(format!("bad request: {e}")))?;
            if req.needs_descriptors() {
                return Err(Error::Invalid(format!("{} streams data; use the /1.0/exec or /1.0/images endpoint", req.verb())));
            }
            self.daemon_call(daemon, &Request::As { caller, request: Box::new(req) })
        })();
        match result {
            Ok(r) => (200, r),
            Err(Error::FrameTooLarge(n)) => (413, Response::Error { code: 2, message: format!("request body of {n} bytes is too large") }),
            Err(e) => error_response(&e),
        }
    }

    fn still_trusted(&self, fingerprint: Option<&str>) -> bool {
        match fingerprint {
            Some(f) => {
                self.forget(f);
                self.lookup(f).is_some()
            }
            None => false,
        }
    }

    fn exec(&self, mut conn: Conn, head: &Head, caller: Caller, peer: &Peer) -> Result<()> {
        let key = match head.header("sec-websocket-key") {
            Some(k) if head.has_token("upgrade", "websocket") => k.to_string(),
            _ => {
                write_json(&mut conn, 400, &Response::Error { code: 2, message: "exec needs a WebSocket upgrade".into() }, false)?;
                return Ok(());
            }
        };
        let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
        conn.write_all(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .as_bytes(),
        )?;
        conn.flush()?;
        let mut ws = WebSocket::from_raw_socket(conn, WsRole::Server, None);
        let exit = |ws: &mut WebSocket<Conn>, value: serde_json::Value| {
            let mut frame = vec![CHANNEL_EXIT];
            frame.extend_from_slice(value.to_string().as_bytes());
            let _ = ws.send(Message::binary(frame));
            let _ = ws.close(None);
            let _ = ws.flush();
        };
        let fail = |ws: &mut WebSocket<Conn>, e: &Error| exit(ws, serde_json::json!({"error": e.payload(), "code": e.exit_code()}));
        let first = match ws.read() {
            Ok(Message::Text(t)) => {
                serde_json::from_str::<Request>(t.as_str()).map_err(|e| Error::Invalid(format!("bad exec request: {e}")))
            }
            Ok(Message::Binary(b)) => serde_json::from_slice::<Request>(&b).map_err(|e| Error::Invalid(format!("bad exec request: {e}"))),
            Ok(_) => Err(Error::Invalid("the first exec message must be the request".into())),
            Err(e) => Err(Error::Unreachable(e.to_string())),
        };
        let req = match first {
            Ok(r @ Request::Exec { .. }) => r,
            Ok(other) => {
                fail(&mut ws, &Error::Invalid(format!("{} cannot be streamed", other.verb())));
                return Ok(());
            }
            Err(e) => {
                fail(&mut ws, &e);
                return Ok(());
            }
        };
        let pairs = (UnixStream::pair()?, UnixStream::pair()?, UnixStream::pair()?);
        let ((mut stdin, stdin_remote), (stdout, stdout_remote), (stderr, stderr_remote)) = pairs;
        let mut daemon = match self.daemon() {
            Ok(d) => d,
            Err(e) => {
                fail(&mut ws, &e);
                return Ok(());
            }
        };
        let request = Request::As { caller, request: Box::new(req) };
        if let Err(e) = daemon.send_with_fds(&request, &[&stdin_remote.as_raw_fd(), &stdout_remote.as_raw_fd(), &stderr_remote.as_raw_fd()])
        {
            fail(&mut ws, &e);
            return Ok(());
        }
        drop((stdin_remote, stdout_remote, stderr_remote));
        let (exit_tx, exit_rx) = mpsc::channel::<Result<Response>>();
        std::thread::spawn(move || {
            let _ = exit_tx.send(daemon.read_response());
        });
        let (out_tx, out_rx) = mpsc::channel::<(u8, Vec<u8>)>();
        for (channel, mut stream) in [(CHANNEL_STDOUT, stdout), (CHANNEL_STDERR, stderr)] {
            let tx = out_tx.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 32768];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => {
                            let _ = tx.send((channel, Vec::new()));
                            return;
                        }
                        Ok(n) => {
                            if tx.send((channel, buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
        drop(out_tx);
        ws.get_mut().get_mut().sock.set_read_timeout(Some(POLL))?;
        let mut open_outputs = 2;
        let mut result: Option<(Result<Response>, Instant)> = None;
        let mut last_trust = Instant::now();
        loop {
            match ws.read() {
                Ok(Message::Binary(b)) if !b.is_empty() => match b[0] {
                    CHANNEL_STDIN => {
                        let _ = stdin.write_all(&b[1..]);
                    }
                    CHANNEL_CONTROL => {
                        let v: serde_json::Value = serde_json::from_slice(&b[1..]).unwrap_or_default();
                        if v["eof"].as_bool() == Some(true) {
                            let _ = stdin.shutdown(Shutdown::Write);
                        }
                    }
                    _ => {}
                },
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => return Ok(()),
            }
            while let Ok((channel, data)) = out_rx.try_recv() {
                if data.is_empty() {
                    open_outputs -= 1;
                    continue;
                }
                let mut frame = vec![channel];
                frame.extend_from_slice(&data);
                if ws.send(Message::binary(frame)).is_err() {
                    return Ok(());
                }
            }
            if result.is_none() {
                if let Ok(r) = exit_rx.try_recv() {
                    result = Some((r, Instant::now()));
                }
            }
            if let Some((r, at)) = &result {
                if open_outputs == 0 || at.elapsed() > Duration::from_secs(2) {
                    match into_result(match r {
                        Ok(resp) => resp.clone(),
                        Err(e) => Response::error(e),
                    }) {
                        Ok(Response::Exit { status }) => exit(&mut ws, serde_json::json!({"status": status})),
                        Ok(other) => fail(&mut ws, &Error::Internal(format!("unexpected response {other:?}"))),
                        Err(e) => fail(&mut ws, &e),
                    }
                    return Ok(());
                }
            }
            if last_trust.elapsed() > TRUST_TTL {
                last_trust = Instant::now();
                if !self.still_trusted(peer.fingerprint.as_deref()) {
                    fail(&mut ws, &Error::Forbidden("this client certificate is no longer trusted".into()));
                    return Ok(());
                }
            }
        }
    }

    fn images(&self, mut conn: Conn, head: &Head, caller: Caller) -> Result<()> {
        let result = (|| -> Result<Response> {
            let (mut local, remote) = UnixStream::pair()?;
            let mut daemon = self.daemon()?;
            daemon.send_with_fds(&Request::As { caller, request: Box::new(Request::ImageImport) }, &[&remote.as_raw_fd()])?;
            drop(remote);
            let mut body = Body::new(&mut conn, head, true)?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = body.read(&mut buf)?;
                if n == 0 || local.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            drop(local);
            daemon.read_response()
        })();
        let (status, resp) = match result {
            Ok(r) => (200, r),
            Err(e) => error_response(&e),
        };
        write_json(&mut conn, status, &resp, false)
    }

    fn logs(&self, mut conn: Conn, head: &Head, caller: Caller, peer: &Peer) -> Result<()> {
        Body::new(&mut conn, head, true)?.drain()?;
        let Some(target) = head.param("target").map(String::from) else {
            return write_json(&mut conn, 400, &Response::Error { code: 2, message: "logs needs a target parameter".into() }, false);
        };
        let follow = matches!(head.param("follow"), Some("1" | "true" | "yes"));
        let mut tail = head.param("tail").and_then(|t| t.parse::<usize>().ok());
        let services: Vec<String> =
            head.param("service").map(|s| s.split(',').filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default();
        let raw = matches!(head.param("raw"), Some("1" | "true" | "yes"));
        let mut source = LogSource::from_flags(raw, &services);
        let mut daemon: Option<Client<UnixStream>> = None;
        let as_caller = |request: Request| Request::As { caller: caller.clone(), request: Box::new(request) };
        let mut offset: Option<u64> = None;
        let mut started = false;
        let mut last_trust = Instant::now();
        let mut finishing = false;
        loop {
            let req = as_caller(Request::Logs { target: target.clone(), tail: tail.take(), offset, source, services: services.clone() });
            match self.daemon_call(&mut daemon, &req) {
                Ok(Response::Log { data, next_offset, source: s }) => {
                    if !started {
                        write_chunked_head(&mut conn, 200, "text/plain; charset=utf-8")?;
                        started = true;
                    }
                    source = s;
                    offset = Some(next_offset);
                    if write_chunk(&mut conn, data.as_bytes()).is_err() {
                        return Ok(());
                    }
                    if !follow || finishing {
                        return finish_chunks(&mut conn);
                    }
                    if data.is_empty() {
                        let running = match self.daemon_call(&mut daemon, &as_caller(Request::Ps { all: true, project: None })) {
                            Ok(Response::Containers(list)) => list
                                .iter()
                                .any(|c| (c.name == target || c.id.to_string().starts_with(&target)) && c.state == State::Running),
                            _ => false,
                        };
                        if !running {
                            finishing = true;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                Ok(other) => {
                    let e = Error::Internal(format!("unexpected response {other:?}"));
                    return if started { finish_chunks(&mut conn) } else { write_json(&mut conn, 500, &Response::error(&e), false) };
                }
                Err(e) => {
                    if started {
                        let _ = write_chunk(&mut conn, format!("\ncollocate: {}\n", e.payload()).as_bytes());
                        return finish_chunks(&mut conn);
                    }
                    let (status, body) = error_response(&e);
                    return write_json(&mut conn, status, &body, false);
                }
            }
            if last_trust.elapsed() > TRUST_TTL {
                last_trust = Instant::now();
                if !self.still_trusted(peer.fingerprint.as_deref()) {
                    let _ = write_chunk(&mut conn, b"\ncollocate: this client certificate is no longer trusted\n");
                    return finish_chunks(&mut conn);
                }
            }
        }
    }
}
