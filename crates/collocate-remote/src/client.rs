use crate::http::{read_response, write_request_head, Body, Buffered};
use crate::tls::{client_config, server_name};
use crate::{CHANNEL_CONTROL, CHANNEL_EXIT, CHANNEL_STDERR, CHANNEL_STDIN, CHANNEL_STDOUT};
use collocate_core::client::{into_result, Api};
use collocate_core::request::{Request, Response};
use collocate_core::{Error, Result};
use collocate_trust::{Identity, Token};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::Duration;
use tungstenite::Message;

pub type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub addresses: Vec<String>,
    pub fingerprint: String,
}

fn unreachable(e: impl std::fmt::Display) -> Error {
    Error::Unreachable(e.to_string())
}

pub fn dial(endpoint: &Endpoint, identity: &Identity) -> Result<TlsStream> {
    let config = client_config(identity, &endpoint.fingerprint)?;
    let mut errors = Vec::new();
    for address in &endpoint.addresses {
        let attempt = || -> Result<TlsStream> {
            let sockaddr =
                address.to_socket_addrs().map_err(unreachable)?.next().ok_or_else(|| unreachable(format!("{address} does not resolve")))?;
            let sock = TcpStream::connect_timeout(&sockaddr, CONNECT_TIMEOUT).map_err(unreachable)?;
            sock.set_nodelay(true)?;
            let conn =
                rustls::ClientConnection::new(config.clone(), server_name(address)?).map_err(|e| Error::Internal(format!("tls: {e}")))?;
            let mut tls = rustls::StreamOwned::new(conn, sock);
            while tls.conn.is_handshaking() {
                tls.conn.complete_io(&mut tls.sock).map_err(unreachable)?;
            }
            Ok(tls)
        };
        match attempt() {
            Ok(t) => return Ok(t),
            Err(e) => errors.push(format!("{address}: {}", e.payload())),
        }
    }
    Err(Error::Unreachable(errors.join("; ")))
}

fn error_from(status: u16, body: &[u8]) -> Error {
    match serde_json::from_slice::<Response>(body) {
        Ok(r @ Response::Error { .. }) => into_result(r).err().unwrap_or_else(|| Error::Internal(format!("HTTP {status}"))),
        _ => Error::Internal(format!("HTTP {status}: {}", String::from_utf8_lossy(body).trim())),
    }
}

pub struct HttpsApi {
    endpoint: Endpoint,
    identity: Identity,
    conn: Option<Buffered<TlsStream>>,
}

impl HttpsApi {
    pub fn new(endpoint: Endpoint, identity: Identity) -> HttpsApi {
        HttpsApi { endpoint, identity, conn: None }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn connection(&mut self) -> Result<&mut Buffered<TlsStream>> {
        if self.conn.is_none() {
            self.conn = Some(Buffered::new(dial(&self.endpoint, &self.identity)?));
        }
        Ok(self.conn.as_mut().expect("connection"))
    }

    fn exchange(&mut self, method: &str, path: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        let conn = self.connection()?;
        write_request_head(conn, method, path, &[("Content-Type", "application/json".into()), ("Content-Length", body.len().to_string())])?;
        conn.write_all(body)?;
        conn.flush()?;
        let head = read_response(conn)?;
        let data = Body::new(conn, &head, false)?.read_all(MAX_RESPONSE)?;
        if !head.keep_alive() {
            self.conn = None;
        }
        Ok((head.status, data))
    }

    pub fn request(&mut self, method: &str, path: &str, body: &[u8]) -> Result<(u16, Vec<u8>)> {
        let reused = self.conn.is_some();
        match self.exchange(method, path, body) {
            Err(Error::Eof) | Err(Error::Io(_)) if reused => {
                self.conn = None;
                self.exchange(method, path, body)
            }
            Err(e) => {
                self.conn = None;
                Err(e)
            }
            ok => ok,
        }
    }

    pub fn server_info(&mut self) -> Result<serde_json::Value> {
        let (status, body) = self.request("GET", "/1.0", b"")?;
        if status != 200 {
            return Err(error_from(status, &body));
        }
        Ok(serde_json::from_slice(&body)?)
    }

    pub fn import(&mut self, reader: &mut dyn Read, length: Option<u64>) -> Result<Response> {
        self.conn = None;
        let mut tls = dial(&self.endpoint, &self.identity)?;
        let framing = match length {
            Some(n) => ("Content-Length", n.to_string()),
            None => ("Transfer-Encoding", "chunked".to_string()),
        };
        write_request_head(
            &mut tls,
            "POST",
            "/1.0/images",
            &[("Content-Type", "application/x-tar".into()), framing, ("Connection", "close".into())],
        )?;
        let mut chunk = vec![0u8; 256 * 1024];
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            if length.is_some() {
                tls.write_all(&chunk[..n])?;
            } else {
                crate::http::write_chunk(&mut tls, &chunk[..n])?;
            }
        }
        if length.is_none() {
            crate::http::finish_chunks(&mut tls)?;
        }
        tls.flush()?;
        let mut conn = Buffered::new(tls);
        let head = read_response(&mut conn)?;
        let body = Body::new(&mut conn, &head, false)?.read_all(MAX_RESPONSE)?;
        let resp: Response = serde_json::from_slice(&body).map_err(|_| error_from(head.status, &body))?;
        into_result(resp)
    }

    pub fn exec(&mut self, req: &Request, stdin: Box<dyn Read + Send>, stdout: &mut dyn Write, stderr: &mut dyn Write) -> Result<i32> {
        if !matches!(req, Request::Exec { .. }) {
            return Err(Error::Invalid("only exec requests can be streamed".into()));
        }
        let tls = dial(&self.endpoint, &self.identity)?;
        let (mut ws, _) =
            tungstenite::client::client("wss://collocate/1.0/exec", tls).map_err(|e| Error::Unreachable(format!("exec handshake: {e}")))?;
        ws.send(Message::text(serde_json::to_string(req)?)).map_err(ws_error)?;
        ws.get_mut().sock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        std::thread::spawn(move || {
            let mut input = stdin;
            let mut buf = vec![0u8; 16384];
            loop {
                match input.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(None);
                        return;
                    }
                    Ok(n) => {
                        if tx.send(Some(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut stdin_open = true;
        loop {
            match ws.read() {
                Ok(Message::Binary(b)) if !b.is_empty() => match b[0] {
                    CHANNEL_STDOUT => {
                        stdout.write_all(&b[1..])?;
                        stdout.flush()?;
                    }
                    CHANNEL_STDERR => {
                        stderr.write_all(&b[1..])?;
                        stderr.flush()?;
                    }
                    CHANNEL_EXIT => {
                        let v: serde_json::Value = serde_json::from_slice(&b[1..])?;
                        let _ = ws.close(None);
                        if let Some(message) = v["error"].as_str() {
                            return Err(into_result(Response::Error {
                                code: v["code"].as_i64().unwrap_or(1) as i32,
                                message: message.to_string(),
                            })
                            .err()
                            .unwrap_or_else(|| Error::Internal(message.to_string())));
                        }
                        return Ok(v["status"].as_i64().unwrap_or(1) as i32);
                    }
                    _ => {}
                },
                Ok(Message::Close(_)) => return Err(Error::Unreachable("the gateway closed the exec stream".into())),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(e) => return Err(ws_error(e)),
            }
            while stdin_open {
                match rx.try_recv() {
                    Ok(Some(data)) => {
                        let mut frame = vec![CHANNEL_STDIN];
                        frame.extend_from_slice(&data);
                        ws.send(Message::binary(frame)).map_err(ws_error)?;
                    }
                    Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                        let mut frame = vec![CHANNEL_CONTROL];
                        frame.extend_from_slice(br#"{"eof":true}"#);
                        ws.send(Message::binary(frame)).map_err(ws_error)?;
                        stdin_open = false;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
    }
}

fn ws_error(e: tungstenite::Error) -> Error {
    Error::Unreachable(format!("exec stream: {e}"))
}

impl Api for HttpsApi {
    fn call(&mut self, req: Request) -> Result<Response> {
        let body = serde_json::to_vec(&req)?;
        let (status, data) = self.request("POST", "/1.0/call", &body)?;
        match serde_json::from_slice::<Response>(&data) {
            Ok(r) => into_result(r),
            Err(_) => Err(error_from(status, &data)),
        }
    }
}

pub fn enroll(token: &Token, identity: &Identity, name: Option<&str>) -> Result<serde_json::Value> {
    let mut api = HttpsApi::new(Endpoint { addresses: token.addresses.clone(), fingerprint: token.fingerprint.clone() }, identity.clone());
    let body = serde_json::to_vec(&serde_json::json!({"token": token.encode()?, "name": name}))?;
    let (status, data) = api.request("POST", "/1.0/certificates", &body)?;
    if status != 201 {
        return Err(error_from(status, &data));
    }
    Ok(serde_json::from_slice(&data)?)
}
