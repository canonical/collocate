use collocate_core::{Error, Result};
use std::io::{Read, Write};

pub const MAX_HEAD: usize = 64 * 1024;

pub struct Buffered<S> {
    inner: S,
    buf: Vec<u8>,
    pos: usize,
}

impl<S> Buffered<S> {
    pub fn new(inner: S) -> Buffered<S> {
        Buffered { inner, buf: Vec::new(), pos: 0 }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    pub fn pending(&self) -> &[u8] {
        &self.buf[self.pos..]
    }
}

impl<S: Read> Read for Buffered<S> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos < self.buf.len() {
            let n = out.len().min(self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            if self.pos == self.buf.len() {
                self.buf.clear();
                self.pos = 0;
            }
            return Ok(n);
        }
        self.inner.read(out)
    }
}

impl<S: Write> Write for Buffered<S> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.inner.write(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn find_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

impl<S: Read> Buffered<S> {
    fn read_head_bytes(&mut self, max: usize) -> Result<Option<Vec<u8>>> {
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        loop {
            if let Some(end) = find_end(&self.buf) {
                let head: Vec<u8> = self.buf.drain(..end).collect();
                return Ok(Some(head));
            }
            if self.buf.len() > max {
                return Err(Error::Invalid("request headers are too large".into()));
            }
            let mut chunk = [0u8; 4096];
            let n = self.inner.read(&mut chunk)?;
            if n == 0 {
                return if self.buf.is_empty() { Ok(None) } else { Err(Error::Eof) };
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub http10: bool,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.header(name).is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    }

    pub fn content_length(&self) -> Result<Option<u64>> {
        self.header("content-length").map(|v| v.trim().parse::<u64>().map_err(|_| Error::Invalid("bad content-length".into()))).transpose()
    }

    pub fn chunked(&self) -> bool {
        self.has_token("transfer-encoding", "chunked")
    }

    pub fn keep_alive(&self) -> bool {
        if self.http10 {
            self.has_token("connection", "keep-alive")
        } else {
            !self.has_token("connection", "close")
        }
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn split_query(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    (path.to_string(), pairs)
}

fn headers_of(raw: &[httparse::Header<'_>]) -> Vec<(String, String)> {
    raw.iter().map(|h| (h.name.to_string(), String::from_utf8_lossy(h.value).trim().to_string())).collect()
}

pub fn read_request<S: Read>(c: &mut Buffered<S>) -> Result<Option<Head>> {
    let Some(bytes) = c.read_head_bytes(MAX_HEAD)? else { return Ok(None) };
    let mut raw = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut raw);
    match req.parse(&bytes) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(Error::Invalid("malformed HTTP request".into())),
    }
    let (path, query) = split_query(req.path.unwrap_or("/"));
    Ok(Some(Head {
        method: req.method.unwrap_or("").to_string(),
        path,
        query,
        status: 0,
        headers: headers_of(req.headers),
        http10: req.version == Some(0),
    }))
}

pub fn read_response<S: Read>(c: &mut Buffered<S>) -> Result<Head> {
    let bytes = c.read_head_bytes(MAX_HEAD)?.ok_or(Error::Eof)?;
    let mut raw = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut raw);
    match resp.parse(&bytes) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(Error::Invalid("malformed HTTP response".into())),
    }
    Ok(Head {
        method: String::new(),
        path: String::new(),
        query: Vec::new(),
        status: resp.code.unwrap_or(0),
        headers: headers_of(resp.headers),
        http10: resp.version == Some(0),
    })
}

enum Framing {
    Length(u64),
    Chunked { remaining: u64, done: bool },
    UntilClose,
}

pub struct Body<'a, S> {
    conn: &'a mut Buffered<S>,
    framing: Framing,
}

impl<'a, S: Read> Body<'a, S> {
    pub fn new(conn: &'a mut Buffered<S>, head: &Head, request: bool) -> Result<Body<'a, S>> {
        let framing = if head.chunked() {
            Framing::Chunked { remaining: 0, done: false }
        } else if let Some(n) = head.content_length()? {
            Framing::Length(n)
        } else if request {
            Framing::Length(0)
        } else {
            Framing::UntilClose
        };
        Ok(Body { conn, framing })
    }

    fn read_line(&mut self) -> std::io::Result<String> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            if self.conn.read(&mut byte)? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            if byte[0] == b'\n' {
                break;
            }
            if line.len() > 4096 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "chunk header too long"));
            }
            line.push(byte[0]);
        }
        Ok(String::from_utf8_lossy(&line).trim().to_string())
    }

    pub fn read_all(&mut self, limit: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            let n = self.read(&mut chunk)?;
            if n == 0 {
                return Ok(out);
            }
            if out.len() + n > limit {
                return Err(Error::FrameTooLarge(out.len() + n));
            }
            out.extend_from_slice(&chunk[..n]);
        }
    }

    pub fn drain(&mut self) -> Result<()> {
        let mut chunk = [0u8; 16384];
        while self.read(&mut chunk)? > 0 {}
        Ok(())
    }
}

impl<S: Read> Read for Body<'_, S> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self.framing {
            Framing::Length(0) => Ok(0),
            Framing::Length(left) => {
                let want = out.len().min(left.min(usize::MAX as u64) as usize);
                let n = self.conn.read(&mut out[..want])?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                self.framing = Framing::Length(left - n as u64);
                Ok(n)
            }
            Framing::UntilClose => self.conn.read(out),
            Framing::Chunked { done: true, .. } => Ok(0),
            Framing::Chunked { remaining, .. } => {
                let mut remaining = remaining;
                if remaining == 0 {
                    let line = self.read_line()?;
                    let size = line.split(';').next().unwrap_or("");
                    remaining = u64::from_str_radix(size.trim(), 16)
                        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size"))?;
                    if remaining == 0 {
                        loop {
                            if self.read_line()?.is_empty() {
                                break;
                            }
                        }
                        self.framing = Framing::Chunked { remaining: 0, done: true };
                        return Ok(0);
                    }
                }
                let want = out.len().min(remaining.min(usize::MAX as u64) as usize);
                let n = self.conn.read(&mut out[..want])?;
                if n == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                remaining -= n as u64;
                if remaining == 0 {
                    self.read_line()?;
                }
                self.framing = Framing::Chunked { remaining, done: false };
                Ok(n)
            }
        }
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

pub fn write_response<W: Write>(w: &mut W, status: u16, content_type: &str, body: &[u8], keep_alive: bool) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: {}\r\n\r\n",
        reason(status),
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    w.write_all(head.as_bytes())?;
    w.write_all(body)?;
    w.flush()?;
    Ok(())
}

pub fn write_json<W: Write, T: serde::Serialize>(w: &mut W, status: u16, value: &T, keep_alive: bool) -> Result<()> {
    write_response(w, status, "application/json", &serde_json::to_vec(value)?, keep_alive)
}

pub fn write_chunked_head<W: Write>(w: &mut W, status: u16, content_type: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        reason(status)
    );
    w.write_all(head.as_bytes())?;
    w.flush()?;
    Ok(())
}

pub fn write_chunk<W: Write>(w: &mut W, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    w.write_all(format!("{:x}\r\n", data.len()).as_bytes())?;
    w.write_all(data)?;
    w.write_all(b"\r\n")?;
    w.flush()?;
    Ok(())
}

pub fn finish_chunks<W: Write>(w: &mut W) -> Result<()> {
    w.write_all(b"0\r\n\r\n")?;
    w.flush()?;
    Ok(())
}

pub fn write_request_head<W: Write>(w: &mut W, method: &str, path: &str, extra: &[(&str, String)]) -> Result<()> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: collocate\r\nUser-Agent: collocate/{}\r\n", env!("CARGO_PKG_VERSION"));
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    w.write_all(head.as_bytes())?;
    Ok(())
}
